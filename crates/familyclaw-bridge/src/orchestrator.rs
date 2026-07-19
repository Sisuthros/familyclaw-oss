//! Multi-agent orchestration: a DAG-based workflow engine (director→worker).
//!
//! This module builds **solely** on the bridge's public interface
//! ([`crate::bridge::FamilyBridge`]): the agent registry, task board, and
//! event bus. It models a workflow as a directed acyclic graph
//! ([`OrchestrationPlan`]), where each node ([`TaskNode`]) turns into a
//! concrete [`crate::task::Task`] on the task board at run time.
//!
//! ## Design principles
//! - **Only legal state transitions.** The orchestrator drives tasks only
//!   along transitions permitted by the frozen [`crate::task::TaskStatus`]
//!   state machine (`Pending → Active → Done`), so that durable replay stays
//!   intact.
//! - **Determinism.** Every time-dependent decision (liveness) takes a `now`
//!   parameter; the same plan and the same `now` value always yield the same
//!   work order and the same worker selection. The system clock is never
//!   read.
//! - **No extension of core types.** Coordination events are published as
//!   [`crate::event::EventKind::Custom`] with the prefix `orchestration.`.
//! - **Bounded sub-delegation.** Recursive execution of sub-workflows is
//!   capped by a depth budget (cf. an iteration budget for nested agent
//!   calls).
//!
//! ## Example
//! ```
//! use familyclaw_bridge::{
//!     AgentInfo, AgentRole, FamilyBridge, HostKind, NodeId, OrchestrationPlan,
//!     Orchestrator, TaskNode, TaskStatus,
//! };
//! use familyclaw_core::ids::AgentId;
//! use familyclaw_core::time;
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let worker = AgentInfo::new(AgentId::new(), "w", AgentRole::Executor, HostKind::Local);
//! let wid = worker.id;
//! bridge.register_agent(worker).await?;
//! let now = time::now();
//! bridge.heartbeat(wid, now).await?; // bring the worker online
//!
//! let plan = OrchestrationPlan::new("demo", vec![
//!     TaskNode::new("a", "step a", "do a"),
//! ]);
//! let orch = Orchestrator::new(bridge);
//! let report = orch.run(&plan, now).await?;
//! assert_eq!(report.completed.len(), 1);
//! # Ok(())
//! # }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::Timestamp;
use familyclaw_core::{FamilyClawError, Result};

use crate::agent::{AgentRole, Liveness};
use crate::bridge::FamilyBridge;
use crate::contract::{Capability, ContractBoard, Deliverable};
use crate::event::{Event, EventKind};
use crate::executor::{OrchestratedTurn, TurnExecutor};
use crate::task::{TaskId, TaskStatus};

/// The event the orchestrator publishes when a node's task has been assigned
/// to a worker and activated.
pub const STEP_ASSIGNED: &str = "orchestration.step_assigned";

/// The event the orchestrator publishes when the entire workflow is complete
/// (all nodes in the [`TaskStatus::Done`] state).
pub const WORKFLOW_DONE: &str = "orchestration.workflow_done";

/// The event the orchestrator publishes when a node's turn **fails**: the
/// executor returned an error, or the deliverable breached the node's
/// capability contract (output schema/postcondition). The node's task is left
/// in a non-`Done` state and its descendants are not advanced — the
/// [`TaskStatus`] state machine has no `Failed` value, so failure is
/// expressed via this [`EventKind::Custom`] event.
pub const STEP_FAILED: &str = "orchestration.step_failed";

/// The depth cap for recursive sub-delegation (cf. an iteration budget).
///
/// An [`Orchestrator::run_nested`] call that exceeds this returns an error
/// instead of running unboundedly deeper.
pub const MAX_DELEGATION_DEPTH: usize = 4;

/// The stable identifier of a single workflow node (a human-readable string).
///
/// Unlike UUID-based [`crate::task::TaskId`] values, `NodeId` is a name given
/// by the planner (e.g. `"build"`, `"test"`), so dependencies are written
/// readably and the topological order is stable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl NodeId {
    /// Builds an identifier from any value convertible to a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for NodeId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A single workflow node: one delegatable work step.
///
/// A node describes *what* is done and *who it fits* (role + capabilities),
/// and *after what* it can start ([`deps`](Self::deps)). At run time, a node
/// turns into a single task on the task board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNode {
    /// The node's stable identifier within the workflow (must be unique).
    pub id: NodeId,

    /// Short title (becomes the task's title).
    pub title: String,

    /// Free-form description of the work step.
    #[serde(default)]
    pub description: String,

    /// The role required of the worker, or `None` if any role is acceptable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_role: Option<AgentRole>,

    /// The required capabilities: the worker's capability set must be a
    /// superset of these.
    #[serde(default)]
    pub required_capabilities: Vec<String>,

    /// Nodes that must be complete before this one can start.
    #[serde(default)]
    pub deps: Vec<NodeId>,

    /// A pinned worker: if set, selection is bypassed and the task is
    /// assigned directly to this agent (if it is online).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_assignee: Option<AgentId>,

    /// An optional capability/contract against which the node's deliverable
    /// is verified after the execution seam
    /// ([`crate::executor::TurnExecutor`]). If set,
    /// [`Orchestrator::run_with`] runs the deliverable through
    /// [`crate::contract::ContractBoard::fulfill`] verification (output
    /// schema and postconditions) **before** the node is moved to the `Done`
    /// state; a violation marks the node as failed and its descendants are
    /// not advanced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
}

impl TaskNode {
    /// Builds a node with an identifier, title, and description, without
    /// constraints.
    pub fn new(
        id: impl Into<NodeId>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            required_role: None,
            required_capabilities: Vec::new(),
            deps: Vec::new(),
            pinned_assignee: None,
            capability: None,
        }
    }

    /// Sets the required role (builder style).
    #[must_use]
    pub fn with_role(mut self, role: AgentRole) -> Self {
        self.required_role = Some(role);
        self
    }

    /// Sets the required capabilities (builder style).
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, caps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_capabilities = caps.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the dependencies (builder style).
    #[must_use]
    pub fn with_deps<I, N>(mut self, deps: I) -> Self
    where
        I: IntoIterator<Item = N>,
        N: Into<NodeId>,
    {
        self.deps = deps.into_iter().map(Into::into).collect();
        self
    }

    /// Pins the worker (builder style).
    #[must_use]
    pub fn with_pinned_assignee(mut self, agent: AgentId) -> Self {
        self.pinned_assignee = Some(agent);
        self
    }

    /// Attaches a verifiable capability/contract to the node (builder style).
    ///
    /// When a node has a capability, [`Orchestrator::run_with`] runs the
    /// deliverable produced by the execution seam through
    /// [`crate::contract::ContractBoard::fulfill`] verification before the
    /// `Done` transition.
    #[must_use]
    pub fn with_capability(mut self, capability: Capability) -> Self {
        self.capability = Some(capability);
        self
    }
}

/// A directed acyclic workflow description (DAG).
///
/// A plan is assembled from nodes whose dependencies between each other
/// ([`TaskNode::deps`]) form a graph. [`validate`](Self::validate) verifies
/// that the graph is valid (no cycles, no dangling dependencies, no duplicate
/// identifiers), and [`topo_order`](Self::topo_order) returns a deterministic
/// execution order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationPlan {
    /// The plan's human-readable identifier.
    pub id: String,

    /// The workflow's nodes.
    pub nodes: Vec<TaskNode>,
}

impl OrchestrationPlan {
    /// Builds a plan with an identifier and nodes (without validating).
    pub fn new(id: impl Into<String>, nodes: Vec<TaskNode>) -> Self {
        Self {
            id: id.into(),
            nodes,
        }
    }

    /// Looks up a node by identifier.
    #[must_use]
    pub fn node(&self, id: &NodeId) -> Option<&TaskNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// Validates the plan's structure.
    ///
    /// Rejects:
    /// - a **duplicate identifier** (the same [`NodeId`] twice),
    /// - a **dangling dependency** (a dep points to an unknown node),
    /// - a **cycle** (the graph has a loop — topological ordering fails).
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] with a descriptive message if any of
    /// the conditions above is violated.
    pub fn validate(&self) -> Result<()> {
        // 1) Duplicate identifiers.
        let mut seen: HashSet<&NodeId> = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !seen.insert(&node.id) {
                return Err(FamilyClawError::invalid_input(format!(
                    "duplicate node id: {}",
                    node.id
                )));
            }
        }

        // 2) Dangling dependencies + self-reference.
        for node in &self.nodes {
            for dep in &node.deps {
                if !seen.contains(dep) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on unknown node {}",
                        node.id, dep
                    )));
                }
                if dep == &node.id {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on itself",
                        node.id
                    )));
                }
            }
        }

        // 3) Cycles: topo-sort only succeeds for an acyclic graph.
        self.topo_order()?;
        Ok(())
    }

    /// Returns a deterministic topological execution order.
    ///
    /// Uses Kahn's algorithm, where ties (multiple nodes ready to run at the
    /// same time) are broken by [`NodeId`] in ascending order — so the order
    /// is the same on every run.
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] if a dependency points to an
    ///   unknown node (a dangling dep).
    /// - [`FamilyClawError::InvalidInput`] if the graph has a cycle.
    pub fn topo_order(&self) -> Result<Vec<NodeId>> {
        let index: HashMap<&NodeId, &TaskNode> = self.nodes.iter().map(|n| (&n.id, n)).collect();

        // Build outgoing edges (dep → dependent) + in-degrees.
        let mut in_degree: HashMap<&NodeId, usize> =
            self.nodes.iter().map(|n| (&n.id, 0usize)).collect();
        let mut dependents: HashMap<&NodeId, Vec<&NodeId>> =
            self.nodes.iter().map(|n| (&n.id, Vec::new())).collect();

        for node in &self.nodes {
            for dep in &node.deps {
                if !index.contains_key(dep) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {} depends on unknown node {}",
                        node.id, dep
                    )));
                }
                // dep → node: node's in-degree increases.
                if let Some(d) = in_degree.get_mut(&node.id) {
                    *d += 1;
                }
                if let Some(list) = dependents.get_mut(dep) {
                    list.push(&node.id);
                }
            }
        }

        // Kahn: always take the smallest ready node (deterministic tie-break).
        let mut ready: Vec<&NodeId> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        ready.sort();
        let mut queue: VecDeque<&NodeId> = ready.into_iter().collect();

        let mut order: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            let mut newly_ready: Vec<&NodeId> = Vec::new();
            if let Some(children) = dependents.get(id) {
                for child in children {
                    if let Some(d) = in_degree.get_mut(child) {
                        *d -= 1;
                        if *d == 0 {
                            newly_ready.push(child);
                        }
                    }
                }
            }
            // Add newly-ready nodes smallest-first and keep the queue sorted
            // so the order is fully deterministic.
            if !newly_ready.is_empty() {
                let mut rest: Vec<&NodeId> = queue.drain(..).collect();
                rest.extend(newly_ready);
                rest.sort();
                queue = rest.into_iter().collect();
            }
        }

        if order.len() != self.nodes.len() {
            return Err(FamilyClawError::invalid_input(
                "orchestration plan contains a cycle",
            ));
        }
        Ok(order)
    }
}

/// A summary of one worker-selection candidate (internal helper struct).
struct Candidate {
    id: AgentId,
    in_flight: usize,
}

/// The outcome of running a single node (internal).
enum NodeOutcome {
    /// The node completed: the task is `Done`.
    Completed(TaskId),
    /// The node failed: the task stayed in a non-`Done` state, its branch
    /// halts.
    Failed,
}

/// A DAG workflow engine that drives the bridge's task board.
///
/// `Orchestrator` does not own any state itself — it carries a shared
/// [`FamilyBridge`] facade and is therefore `Clone`. Every time-dependent
/// decision (liveness) takes a `now` parameter, for determinism.
#[derive(Debug, Clone)]
pub struct Orchestrator {
    bridge: FamilyBridge,
}

/// A report on running a single plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    /// The plan's identifier.
    pub plan_id: String,

    /// The completed nodes and their task identifiers, in topological order.
    pub completed: Vec<(NodeId, TaskId)>,
}

impl Orchestrator {
    /// Builds an orchestrator around the given bridge.
    #[must_use]
    pub fn new(bridge: FamilyBridge) -> Self {
        Self { bridge }
    }

    /// Access to the underlying bridge.
    #[must_use]
    pub fn bridge(&self) -> &FamilyBridge {
        &self.bridge
    }

    /// Selects the best online worker under the given constraints at time
    /// `now`.
    ///
    /// Selection rules:
    /// 1. The role matches (if `required_role` is given).
    /// 2. The capabilities are a superset: the agent's capabilities contain
    ///    every required capability.
    /// 3. The agent is [`Liveness::Online`] at time `now`.
    ///
    /// Ties are broken deterministically: first by fewest in-flight
    /// (non-terminal) tasks, then by smallest [`AgentId`].
    ///
    /// Returns `None` if no one satisfies the conditions.
    pub async fn select_worker(
        &self,
        required_role: Option<AgentRole>,
        required_caps: &[String],
        now: Timestamp,
    ) -> Option<AgentId> {
        let registry = self.bridge.registry();
        let board = self.bridge.board();
        let agents = registry.list().await;

        let mut candidates: Vec<Candidate> = Vec::new();
        for info in agents {
            // 1) Role.
            if let Some(role) = required_role {
                if info.role != role {
                    continue;
                }
            }
            // 2) Capabilities (superset).
            let has_all = required_caps
                .iter()
                .all(|need| info.capabilities.iter().any(|have| have == need));
            if !has_all {
                continue;
            }
            // 3) Liveness.
            match registry.liveness_at(info.id, now).await {
                Ok(Liveness::Online) => {}
                _ => continue,
            }

            let in_flight = board
                .list_for_assignee(info.id)
                .await
                .into_iter()
                .filter(|t| !t.status.is_terminal())
                .count();
            candidates.push(Candidate {
                id: info.id,
                in_flight,
            });
        }

        candidates
            .into_iter()
            .min_by(|a, b| a.in_flight.cmp(&b.in_flight).then(a.id.cmp(&b.id)))
            .map(|c| c.id)
    }

    /// Runs the workflow to completion, driving the frozen task board.
    ///
    /// Progression: validate the plan, walk the nodes in topological order,
    /// and for each node (whose dependencies are all [`TaskStatus::Done`])
    /// create a task, select a worker, and set `Pending → Active → Done`.
    /// Coordination events are published to the bus.
    ///
    /// This is a **synchronous, in-process** driver: it simulates the
    /// worker's completion of the work (moves the task to `Done`) right
    /// after assignment, because the actual LLM/transport layer is wired in
    /// later via an adapter. What matters is that **only legal state
    /// transitions** are used.
    ///
    /// # Errors
    /// - [`FamilyClawError::InvalidInput`] if the plan is invalid
    ///   (cycle/dangling dep/duplicate identifier).
    /// - [`FamilyClawError::NotFound`] if a pinned worker cannot be found or
    ///   no suitable worker exists (`NotFound` with the node's name).
    /// - Propagates the task board's transition errors.
    pub async fn run(&self, plan: &OrchestrationPlan, now: Timestamp) -> Result<RunReport> {
        self.run_nested(plan, now, 0).await
    }

    /// Like [`run`](Self::run), but tracks the recursive delegation depth.
    ///
    /// Sub-workflows (e.g. a node that itself delegates a sub-workflow) call
    /// this with an increasing `depth` value. When `depth` exceeds
    /// [`MAX_DELEGATION_DEPTH`], the run is aborted with an error to prevent
    /// exceeding the budget.
    ///
    /// This is a backward-compatible entry point: it delegates to
    /// [`run_nested_with`](Self::run_nested_with) with the hermetic
    /// [`MockTurnExecutor`](crate::executor::MockTurnExecutor), so the
    /// simulated in-process completion stays bit-for-bit compatible.
    ///
    /// # Errors
    /// As in [`run`](Self::run), plus [`FamilyClawError::InvalidInput`] if
    /// `depth > MAX_DELEGATION_DEPTH`.
    pub async fn run_nested(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        depth: usize,
    ) -> Result<RunReport> {
        let executor = crate::executor::MockTurnExecutor::default();
        self.run_nested_with(plan, now, depth, &executor).await
    }

    /// Runs the workflow to completion, routing each node's turn through the
    /// given [`TurnExecutor`] seam.
    ///
    /// Unlike [`run`](Self::run) (which simulates completion internally with
    /// [`MockTurnExecutor`](crate::executor::MockTurnExecutor)), this lets the
    /// caller plug in the **real** executor (e.g. the LLM/transport layer)
    /// without changing the orchestrator. For each node ready to run:
    ///
    /// 1. an [`OrchestratedTurn`] is built (the node's title/description +
    ///    chosen executor + injected `now`),
    /// 2. [`TurnExecutor::execute`] is called, which returns a deliverable,
    /// 3. if the node has a capability/contract ([`TaskNode::capability`]),
    ///    the deliverable is run through [`ContractBoard::fulfill`]
    ///    verification (output schema + postconditions) **before** the
    ///    `Done` transition,
    /// 4. an accepted deliverable moves the task `Active → Done`; otherwise
    ///    the node is marked failed (the task stays in a non-`Done` state,
    ///    [`STEP_FAILED`] is published) and its descendants are not advanced.
    ///
    /// Determinism is preserved: the clock is never read; `now` is injected
    /// instead. An [`Err`] returned by the executor (e.g. a transport error)
    /// does not hang: the node is marked failed and its branch halts.
    ///
    /// # Errors
    /// The same error set as [`run`](Self::run): an invalid plan, no
    /// eligible worker, an offline pinned worker, exceeding the depth
    /// budget, or a task board transition error.
    pub async fn run_with(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        executor: &dyn TurnExecutor,
    ) -> Result<RunReport> {
        self.run_nested_with(plan, now, 0, executor).await
    }

    /// [`run_with`](Self::run_with) + recursive delegation depth.
    ///
    /// This is the **actual** orchestration loop through which
    /// [`run`](Self::run), [`run_nested`](Self::run_nested), and
    /// [`run_with`](Self::run_with) all pass.
    ///
    /// # Errors
    /// As in [`run_with`](Self::run_with), plus
    /// [`FamilyClawError::InvalidInput`] if `depth > MAX_DELEGATION_DEPTH`.
    pub async fn run_nested_with(
        &self,
        plan: &OrchestrationPlan,
        now: Timestamp,
        depth: usize,
        executor: &dyn TurnExecutor,
    ) -> Result<RunReport> {
        if depth > MAX_DELEGATION_DEPTH {
            return Err(FamilyClawError::invalid_input(format!(
                "sub-delegation depth {depth} exceeds budget {MAX_DELEGATION_DEPTH}"
            )));
        }

        plan.validate()?;
        let order = plan.topo_order()?;

        let board = self.bridge.board();
        let bus = self.bridge.bus();

        // A node → created-task mapping, so dependency completion can be
        // checked. `failed` collects failed nodes, so their descendants are
        // left un-advanced (the branch halts).
        let mut node_task: HashMap<NodeId, TaskId> = HashMap::with_capacity(order.len());
        let mut completed: Vec<(NodeId, TaskId)> = Vec::with_capacity(order.len());
        let mut failed: HashSet<NodeId> = HashSet::new();

        for node_id in &order {
            let node = plan.node(node_id).ok_or_else(|| {
                FamilyClawError::not_found(format!("node {node_id} vanished from plan"))
            })?;

            // If any dependency failed, this node inherits the failure: its
            // branch has already been cut, so the work is not started.
            if node.deps.iter().any(|dep| failed.contains(dep)) {
                failed.insert(node_id.clone());
                continue;
            }

            // A node is ready to run only if ALL its dependencies are Done.
            // The topological order guarantees they have already been
            // processed, but we still confirm the status from the board (we
            // do not assume it).
            for dep in &node.deps {
                let dep_task = node_task.get(dep).ok_or_else(|| {
                    FamilyClawError::invalid_input(format!(
                        "node {node_id} dependency {dep} was not scheduled"
                    ))
                })?;
                let dep_status = board.get(*dep_task).await.map(|t| t.status);
                if dep_status != Some(TaskStatus::Done) {
                    return Err(FamilyClawError::invalid_input(format!(
                        "node {node_id} dependency {dep} is not Done (was {dep_status:?})"
                    )));
                }
            }

            // Select a worker, run the turn through the seam, and move the
            // node either to the `Done` state or mark it failed. The full
            // per-node logic lives in the `drive_node` helper, so this loop
            // stays readable.
            match self.drive_node(plan, node, node_id, now, executor).await? {
                NodeOutcome::Completed(task_id) => {
                    node_task.insert(node_id.clone(), task_id);
                    completed.push((node_id.clone(), task_id));
                }
                NodeOutcome::Failed => {
                    failed.insert(node_id.clone());
                }
            }
        }

        // The whole workflow is complete (only completed nodes are counted).
        let done_payload = WorkflowDonePayload {
            plan_id: plan.id.clone(),
            node_count: completed.len(),
        };
        let event =
            Event::with_payload(EventKind::Custom(WORKFLOW_DONE.into()), None, &done_payload)
                .unwrap_or_else(|_| Event::new(EventKind::Custom(WORKFLOW_DONE.into()), None));
        bus.publish(event);

        Ok(RunReport {
            plan_id: plan.id.clone(),
            completed,
        })
    }

    /// Drives a single node through: selects a worker, publishes
    /// [`STEP_ASSIGNED`], builds the turn, runs it through the
    /// [`TurnExecutor`] seam, and moves the task either to the `Done` state
    /// (an accepted deliverable) or marks it failed ([`STEP_FAILED`], the
    /// task stays in a non-`Done` state).
    ///
    /// # Errors
    /// - [`FamilyClawError::NotFound`] if the pinned worker is not online or
    ///   no suitable worker can be found.
    /// - Propagates the task board's transition/creation errors.
    async fn drive_node(
        &self,
        plan: &OrchestrationPlan,
        node: &TaskNode,
        node_id: &NodeId,
        now: Timestamp,
        executor: &dyn TurnExecutor,
    ) -> Result<NodeOutcome> {
        let board = self.bridge.board();
        let bus = self.bridge.bus();

        // Select a worker: pinned (if online) or rule-based.
        let assignee = match node.pinned_assignee {
            Some(pinned) => match self.bridge.registry().liveness_at(pinned, now).await {
                Ok(Liveness::Online) => pinned,
                _ => {
                    return Err(FamilyClawError::not_found(format!(
                        "pinned worker for node {node_id} is not online"
                    )));
                }
            },
            None => self
                .select_worker(node.required_role, &node.required_capabilities, now)
                .await
                .ok_or_else(|| {
                    FamilyClawError::not_found(format!("no eligible worker for node {node_id}"))
                })?,
        };

        // Create the task, assign it to the worker, and activate it (Pending → Active).
        let task = board.create(node.title.clone(), Some(assignee)).await?;
        board.update_status(task.id, TaskStatus::Active).await?;

        // Publish the assignment event.
        let assigned_payload = StepPayload {
            plan_id: plan.id.clone(),
            node_id: node_id.0.clone(),
            task_id: task.id.to_string(),
            assignee: assignee.to_string(),
        };
        let event = Event::with_payload(
            EventKind::Custom(STEP_ASSIGNED.into()),
            Some(assignee),
            &assigned_payload,
        )
        .unwrap_or_else(|_| Event::new(EventKind::Custom(STEP_ASSIGNED.into()), Some(assignee)));
        bus.publish(event);

        // Build the turn and delegate through the seam. An executor error
        // does NOT hang: it is treated as an unaccepted deliverable.
        let turn = OrchestratedTurn::new(
            plan.id.clone(),
            node_id.clone(),
            task.id,
            assignee,
            node.title.clone(),
            node.description.clone(),
            Self::turn_input(node),
            now,
        );
        let acceptable = match executor.execute(turn).await {
            Ok(deliverable) => {
                Self::deliverable_accepted(node.capability.as_ref(), deliverable, now).await
            }
            Err(_) => false,
        };

        if acceptable {
            // Active → Done (legal).
            board.update_status(task.id, TaskStatus::Done).await?;
            return Ok(NodeOutcome::Completed(task.id));
        }

        // Failed: the task stays in a non-Done state. Publish step_failed.
        let failed_payload = StepFailedPayload {
            plan_id: plan.id.clone(),
            node_id: node_id.0.clone(),
            task_id: task.id.to_string(),
            assignee: assignee.to_string(),
        };
        let event = Event::with_payload(
            EventKind::Custom(STEP_FAILED.into()),
            Some(assignee),
            &failed_payload,
        )
        .unwrap_or_else(|_| Event::new(EventKind::Custom(STEP_FAILED.into()), Some(assignee)));
        bus.publish(event);
        Ok(NodeOutcome::Failed)
    }

    /// Builds the execution seam's machine-readable input from a node.
    ///
    /// Starts with the title and description. If the description parses as a
    /// JSON object, its keys are also lifted to the input's root, so a
    /// structured node input (e.g. `{"brand": "...", "audience": "..."}`)
    /// flows through to the executor as-is.
    fn turn_input(node: &TaskNode) -> serde_json::Value {
        let mut input = serde_json::Map::new();
        input.insert(
            "title".to_string(),
            serde_json::Value::String(node.title.clone()),
        );
        input.insert(
            "description".to_string(),
            serde_json::Value::String(node.description.clone()),
        );
        if let Ok(serde_json::Value::Object(fields)) =
            serde_json::from_str::<serde_json::Value>(&node.description)
        {
            for (k, v) in fields {
                input.insert(k, v);
            }
        }
        serde_json::Value::Object(input)
    }

    /// Verifies the deliverable against the node's capability (if given).
    ///
    /// When `capability` is `None`, any deliverable is accepted (the
    /// simulated path). When a capability is given, it is run through a
    /// one-off [`ContractBoard`] contract: `propose → accept → fulfill`. Only
    /// a full pass (output schema + postconditions) returns `true`; any
    /// violation or contract error returns `false`.
    async fn deliverable_accepted(
        capability: Option<&Capability>,
        deliverable: Deliverable,
        now: Timestamp,
    ) -> bool {
        let Some(capability) = capability else {
            return true;
        };
        let board = ContractBoard::new();
        let provider = deliverable.from;
        // Use an empty input that satisfies the capability's input schema
        // when the schema is empty; otherwise the proposal is validated
        // against the capability's given input schema.
        let proposed = board
            .propose(capability, provider, provider, serde_json::json!({}), now)
            .await;
        let Ok(contract) = proposed else {
            return false;
        };
        if board.accept(contract.id, now).await.is_err() {
            return false;
        }
        board.fulfill(contract.id, deliverable, now).await.is_ok()
    }
}

/// Payload for the `orchestration.step_assigned` event.
#[derive(Debug, Serialize, Deserialize)]
struct StepPayload {
    plan_id: String,
    node_id: String,
    task_id: String,
    assignee: String,
}

/// Payload for the `orchestration.step_failed` event.
#[derive(Debug, Serialize, Deserialize)]
struct StepFailedPayload {
    plan_id: String,
    node_id: String,
    task_id: String,
    assignee: String,
}

/// Payload for the `orchestration.workflow_done` event.
#[derive(Debug, Serialize, Deserialize)]
struct WorkflowDonePayload {
    plan_id: String,
    node_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentInfo, HostKind};
    use familyclaw_core::time;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    async fn online_worker(
        bridge: &FamilyBridge,
        role: AgentRole,
        caps: &[&str],
        now: Timestamp,
    ) -> AgentId {
        let info = AgentInfo::new(AgentId::new(), "w", role, HostKind::Local)
            .with_capabilities(caps.iter().copied());
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat(id, now).await.expect("heartbeat");
        id
    }

    #[test]
    fn validate_rejects_duplicate_node_id() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "t", ""), TaskNode::new("a", "t2", "")],
        );
        let err = plan.validate().expect_err("dup");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_dangling_dependency() {
        let plan =
            OrchestrationPlan::new("p", vec![TaskNode::new("a", "t", "").with_deps(["ghost"])]);
        let err = plan.validate().expect_err("dangling");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_self_dependency() {
        let plan = OrchestrationPlan::new("p", vec![TaskNode::new("a", "t", "").with_deps(["a"])]);
        let err = plan.validate().expect_err("self");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn validate_rejects_cycle() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("a", "ta", "").with_deps(["b"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
            ],
        );
        let err = plan.validate().expect_err("cycle");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn topo_order_is_deterministic_linear() {
        // c -> b -> a (deps), so the order is a, b, c.
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("c", "tc", "").with_deps(["b"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
                TaskNode::new("a", "ta", ""),
            ],
        );
        let order = plan.topo_order().expect("order");
        assert_eq!(
            order,
            vec![NodeId::new("a"), NodeId::new("b"), NodeId::new("c")]
        );
    }

    #[test]
    fn topo_order_ties_break_by_node_id() {
        // a -> {b, c} -> d. b and c are tied → alphabetical order.
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("a", "ta", ""),
                TaskNode::new("c", "tc", "").with_deps(["a"]),
                TaskNode::new("b", "tb", "").with_deps(["a"]),
                TaskNode::new("d", "td", "").with_deps(["b", "c"]),
            ],
        );
        let order = plan.topo_order().expect("order");
        assert_eq!(
            order,
            vec![
                NodeId::new("a"),
                NodeId::new("b"),
                NodeId::new("c"),
                NodeId::new("d")
            ]
        );
    }

    #[test]
    fn topo_order_stable_across_calls() {
        let plan = OrchestrationPlan::new(
            "p",
            vec![
                TaskNode::new("z", "tz", "").with_deps(["a"]),
                TaskNode::new("m", "tm", "").with_deps(["a"]),
                TaskNode::new("a", "ta", ""),
            ],
        );
        let o1 = plan.topo_order().expect("o1");
        let o2 = plan.topo_order().expect("o2");
        assert_eq!(o1, o2);
        assert_eq!(o1[0], NodeId::new("a"));
    }

    #[tokio::test]
    async fn select_worker_matches_role() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let exec = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let _scout = online_worker(&bridge, AgentRole::Scout, &[], now).await;

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, Some(exec));
    }

    #[tokio::test]
    async fn select_worker_requires_capability_superset() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // The agent only has "browser"; "system.run" is also required → does not qualify.
        let _weak = online_worker(&bridge, AgentRole::Executor, &["browser"], now).await;
        let strong = online_worker(
            &bridge,
            AgentRole::Executor,
            &["browser", "system.run"],
            now,
        )
        .await;

        let chosen = bridge_select(
            &bridge,
            Some(AgentRole::Executor),
            &["system.run".to_string()],
            now,
        )
        .await;
        assert_eq!(chosen, Some(strong));
    }

    #[tokio::test]
    async fn select_worker_excludes_offline() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Stale heartbeat → offline at time now.
        let info = AgentInfo::new(AgentId::new(), "old", AgentRole::Executor, HostKind::Local);
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        bridge.heartbeat(id, ts(0)).await.expect("hb"); // 1000s old > 30s timeout

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, None);
    }

    #[tokio::test]
    async fn select_worker_tie_breaks_by_fewest_in_flight_then_id() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        for id in [a, b] {
            let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
            bridge.register_agent(info).await.expect("reg");
            bridge.heartbeat(id, now).await.expect("hb");
        }
        // Give a one in-flight task → b has fewer.
        let t = bridge.create_task("busy", Some(a)).await.expect("task");
        bridge
            .update_task_status(t.id, TaskStatus::Active)
            .await
            .expect("active");

        let chosen = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen, Some(b));

        // Once a's task is complete (terminal), the tie is broken by id → a.
        bridge
            .update_task_status(t.id, TaskStatus::Done)
            .await
            .expect("done");
        let chosen2 = bridge_select(&bridge, Some(AgentRole::Executor), &[], now).await;
        assert_eq!(chosen2, Some(a));
    }

    // Test helper function (Orchestrator::select_worker takes &[String]).
    async fn bridge_select(
        bridge: &FamilyBridge,
        role: Option<AgentRole>,
        caps: &[String],
        now: Timestamp,
    ) -> Option<AgentId> {
        Orchestrator::new(bridge.clone())
            .select_worker(role, caps, now)
            .await
    }

    #[tokio::test]
    async fn run_linear_a_b_c() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let mut sub = bridge.subscribe();

        let plan = OrchestrationPlan::new(
            "linear",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("c", "tc", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["b"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 3);
        assert_eq!(report.completed[0].0, NodeId::new("a"));
        assert_eq!(report.completed[2].0, NodeId::new("c"));

        // All tasks Done.
        for (_node, task_id) in &report.completed {
            let t = bridge.board().get(*task_id).await.expect("task");
            assert_eq!(t.status, TaskStatus::Done);
        }

        // Events received: 3x step_assigned + 1x workflow_done (at least).
        let mut step = 0;
        let mut done = 0;
        while let Ok(Some(ev)) = sub.try_recv() {
            match &ev.kind {
                EventKind::Custom(name) if name == STEP_ASSIGNED => step += 1,
                EventKind::Custom(name) if name == WORKFLOW_DONE => done += 1,
                _ => {}
            }
        }
        assert_eq!(step, 3);
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn run_diamond_a_bc_d() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "diamond",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("c", "tc", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
                TaskNode::new("d", "td", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["b", "c"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 4);
        // a first, d last; b and c in between in alphabetical order.
        let order: Vec<NodeId> = report.completed.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            order,
            vec![
                NodeId::new("a"),
                NodeId::new("b"),
                NodeId::new("c"),
                NodeId::new("d")
            ]
        );
    }

    /// Phase 5 (D3): the orchestrator coordinates **≥2 live agents** by
    /// routing nodes to the right worker based on CAPABILITY. This is genuine
    /// multi-agent coordination that works with the current architecture:
    /// with two workers of different capabilities, different nodes go to
    /// different agents ([`Orchestrator::select_worker`] filters by
    /// `required_capabilities`). The `TurnExecutor` seam (a mock here) is the
    /// same one `LiveTurnExecutor` runs through in production.
    ///
    /// Note (honest scope): nodes are run **sequentially** to completion
    /// (each reaches Done before the next starts), so load balancing alone
    /// does not yet parallelize independent nodes — parallel execution is
    /// part of Phase 5's larger body of work (per-node journal ownership).
    /// Capability-based routing, however, already coordinates ≥2 agents now.
    #[tokio::test]
    async fn run_routes_nodes_to_capable_workers() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // Worker A knows "sql", worker B knows "vision".
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        let info_a = AgentInfo::new(a, "w-sql", AgentRole::Executor, HostKind::Local)
            .with_capabilities(["sql"]);
        let info_b = AgentInfo::new(b, "w-vision", AgentRole::Executor, HostKind::Local)
            .with_capabilities(["vision"]);
        bridge.register_agent(info_a).await.expect("reg a");
        bridge.register_agent(info_b).await.expect("reg b");
        bridge.heartbeat(a, now).await.expect("hb a");
        bridge.heartbeat(b, now).await.expect("hb b");

        // Two nodes: one requires "sql" → A, the other "vision" → B.
        let plan = OrchestrationPlan::new(
            "capability-routed",
            vec![
                TaskNode::new("q", "query", "")
                    .with_role(AgentRole::Executor)
                    .with_capabilities(["sql"]),
                TaskNode::new("img", "analyze", "")
                    .with_role(AgentRole::Executor)
                    .with_capabilities(["vision"]),
            ],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        assert_eq!(report.completed.len(), 2);

        // Each worker got EXACTLY the node matching its capability → ≥2
        // agents were coordinated via capability-based routing.
        let a_tasks = bridge.board().list_for_assignee(a).await.len();
        let b_tasks = bridge.board().list_for_assignee(b).await.len();
        assert_eq!(a_tasks, 1, "sql-työntekijä sai sql-solmun, sai {a_tasks}");
        assert_eq!(
            b_tasks, 1,
            "vision-työntekijä sai vision-solmun, sai {b_tasks}"
        );
    }

    #[tokio::test]
    async fn run_errors_when_no_eligible_worker() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        // No agents at all.
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_role(AgentRole::Executor)],
        );
        let orch = Orchestrator::new(bridge);
        let err = orch.run(&plan, now).await.expect_err("no worker");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn run_uses_pinned_assignee() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let pinned = online_worker(&bridge, AgentRole::Scout, &[], now).await;
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_pinned_assignee(pinned)],
        );
        let orch = Orchestrator::new(bridge.clone());
        let report = orch.run(&plan, now).await.expect("run");
        let (_n, task_id) = &report.completed[0];
        let t = bridge.board().get(*task_id).await.expect("task");
        assert_eq!(t.assignee, Some(pinned));
    }

    #[tokio::test]
    async fn run_pinned_offline_errors() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let info = AgentInfo::new(AgentId::new(), "p", AgentRole::Scout, HostKind::Local);
        let pinned = info.id;
        bridge.register_agent(info).await.expect("reg");
        // No heartbeat → Unknown, not Online.
        let plan = OrchestrationPlan::new(
            "p",
            vec![TaskNode::new("a", "ta", "").with_pinned_assignee(pinned)],
        );
        let orch = Orchestrator::new(bridge);
        let err = orch.run(&plan, now).await.expect_err("offline pinned");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn run_nested_exceeds_depth_budget() {
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let plan = OrchestrationPlan::new("p", vec![TaskNode::new("a", "ta", "")]);
        let orch = Orchestrator::new(bridge);
        let err = orch
            .run_nested(&plan, now, MAX_DELEGATION_DEPTH + 1)
            .await
            .expect_err("over budget");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn run_is_deterministic_same_plan_and_now() {
        let now = ts(1000);
        let make = || async move {
            let bridge = FamilyBridge::new();
            // Two equally eligible workers, fixed ids.
            for n in 1..=2u128 {
                let id = AgentId::from_uuid(uuid::Uuid::from_u128(n));
                let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
                bridge.register_agent(info).await.expect("reg");
                bridge.heartbeat(id, now).await.expect("hb");
            }
            let plan = OrchestrationPlan::new(
                "p",
                vec![
                    TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                    TaskNode::new("b", "tb", "")
                        .with_role(AgentRole::Executor)
                        .with_deps(["a"]),
                ],
            );
            let orch = Orchestrator::new(bridge.clone());
            let report = orch.run(&plan, now).await.expect("run");
            // Return a node→assignee map for comparison.
            let mut out = Vec::new();
            for (node, task_id) in report.completed {
                let t = bridge.board().get(task_id).await.expect("task");
                out.push((node, t.assignee));
            }
            out
        };
        let r1 = make().await;
        let r2 = make().await;
        assert_eq!(r1, r2);
        // The first worker (fewest in-flight, smallest id) is u128=1.
        assert_eq!(r1[0].1, Some(AgentId::from_uuid(uuid::Uuid::from_u128(1))));
    }

    // =======================================================================
    // run_with — the orchestrator routed through the TurnExecutor seam
    // =======================================================================

    use crate::contract::{Capability, Field, FieldType, Schema};
    use crate::executor::{MockFailure, MockTurnExecutor};

    /// A HomepageDesign-shaped result schema that the mock's succeeding
    /// deliverable satisfies but the `failing()` deliverable does not
    /// (missing `headline`).
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
    async fn run_with_mock_executor_runs_linear_plan_to_completion() {
        // run_with + MockTurnExecutor runs the A→B plan to completion: both
        // tasks Done, report order [A, B].
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "linear",
            vec![
                TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::new();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");

        assert_eq!(report.completed.len(), 2);
        assert_eq!(report.completed[0].0, NodeId::new("a"));
        assert_eq!(report.completed[1].0, NodeId::new("b"));
        for (_node, task_id) in &report.completed {
            let t = bridge.board().get(*task_id).await.expect("task");
            assert_eq!(t.status, TaskStatus::Done);
        }
    }

    #[tokio::test]
    async fn run_with_failing_executor_leaves_node_non_done_and_blocks_dependents() {
        // run_with + MockTurnExecutor::failing() (a schema violation) on a node that
        // CARRIES a capability → the node stays non-Done, and its descendant is not advanced.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let worker = online_worker(&bridge, AgentRole::Executor, &[], now).await;
        let mut sub = bridge.subscribe();

        let plan = OrchestrationPlan::new(
            "fails",
            vec![
                TaskNode::new("a", "ta", "")
                    .with_role(AgentRole::Executor)
                    .with_capability(homepage_capability()),
                TaskNode::new("b", "tb", "")
                    .with_role(AgentRole::Executor)
                    .with_deps(["a"]),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        // failing() produces a deliverable without a headline field → fulfill fails.
        let executor = MockTurnExecutor::failing();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");

        // No node completed.
        assert!(report.completed.is_empty(), "no node should complete");

        // A task was created for A but it is NOT Done (stayed in the Active state).
        let a_tasks = bridge
            .board()
            .list_for_assignee(worker)
            .await
            .into_iter()
            .filter(|t| t.title == "ta")
            .collect::<Vec<_>>();
        assert_eq!(a_tasks.len(), 1, "A task was created");
        assert_ne!(a_tasks[0].status, TaskStatus::Done, "A must not be Done");
        assert_eq!(a_tasks[0].status, TaskStatus::Active);

        // B (A's descendant) was never assigned → no task with the title tb.
        let b_tasks = bridge
            .board()
            .list_for_assignee(worker)
            .await
            .into_iter()
            .filter(|t| t.title == "tb")
            .collect::<Vec<_>>();
        assert!(b_tasks.is_empty(), "dependent B must not be scheduled");

        // step_failed was published for A; step_assigned only for A (not B).
        let mut assigned = 0;
        let mut step_failed = 0;
        while let Ok(Some(ev)) = sub.try_recv() {
            match &ev.kind {
                EventKind::Custom(name) if name == STEP_ASSIGNED => assigned += 1,
                EventKind::Custom(name) if name == STEP_FAILED => step_failed += 1,
                _ => {}
            }
        }
        assert_eq!(assigned, 1, "only A is assigned");
        assert_eq!(step_failed, 1, "A emits step_failed");
    }

    #[tokio::test]
    async fn run_delegates_to_run_with_mock_identically() {
        // run() and run_with(MockTurnExecutor::default()) produce
        // the same result — the delegation holds (backward compatibility).
        let now = ts(1000);
        let build = |use_default_run: bool| async move {
            let bridge = FamilyBridge::new();
            for n in 1..=2u128 {
                let id = AgentId::from_uuid(uuid::Uuid::from_u128(n));
                let info = AgentInfo::new(id, "w", AgentRole::Executor, HostKind::Local);
                bridge.register_agent(info).await.expect("reg");
                bridge.heartbeat(id, now).await.expect("hb");
            }
            let plan = OrchestrationPlan::new(
                "p",
                vec![
                    TaskNode::new("a", "ta", "").with_role(AgentRole::Executor),
                    TaskNode::new("b", "tb", "")
                        .with_role(AgentRole::Executor)
                        .with_deps(["a"]),
                ],
            );
            let orch = Orchestrator::new(bridge.clone());
            let report = if use_default_run {
                orch.run(&plan, now).await.expect("run")
            } else {
                let executor = MockTurnExecutor::default();
                orch.run_with(&plan, now, &executor)
                    .await
                    .expect("run_with")
            };
            let mut out = Vec::new();
            for (node, task_id) in report.completed {
                let t = bridge.board().get(task_id).await.expect("task");
                out.push((node, t.status, t.assignee));
            }
            out
        };
        let via_run = build(true).await;
        let via_run_with = build(false).await;
        assert_eq!(via_run, via_run_with);
        assert_eq!(via_run.len(), 2);
        // Both nodes Done on both paths.
        assert!(via_run.iter().all(|(_, s, _)| *s == TaskStatus::Done));
    }

    #[tokio::test]
    async fn run_with_erroring_executor_marks_node_failed_without_hanging() {
        // An executor returning Err → the node fails (not Done), the run
        // returns without hanging. A single-node plan is enough.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "errs",
            vec![TaskNode::new("a", "ta", "").with_role(AgentRole::Executor)],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::with_failure(MockFailure::Error);
        // Ei roikkumista: kutsu palaa Ok-raportilla jossa ei valmistuneita.
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");
        assert!(report.completed.is_empty(), "node must not complete on Err");

        // The task was created and stayed in a non-Done state.
        let tasks = bridge.board().list_for_assignee(w).await;
        assert_eq!(tasks.len(), 1);
        assert_ne!(tasks[0].status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn run_with_capability_node_reaches_done_when_deliverable_valid() {
        // A node with a capability + a succeeding mock → the deliverable
        // passes fulfill → Done. In the description, brand/audience steers
        // the mock to produce a HomepageDesign-shaped (schema-satisfying)
        // deliverable.
        let bridge = FamilyBridge::new();
        let now = ts(1000);
        let _w = online_worker(&bridge, AgentRole::Executor, &[], now).await;

        let plan = OrchestrationPlan::new(
            "ok",
            vec![
                TaskNode::new("a", "ta", r#"{"brand":"DuckUps","audience":"founders"}"#)
                    .with_role(AgentRole::Executor)
                    .with_capability(homepage_capability()),
            ],
        );

        let orch = Orchestrator::new(bridge.clone());
        let executor = MockTurnExecutor::new();
        let report = orch
            .run_with(&plan, now, &executor)
            .await
            .expect("run_with");
        assert_eq!(report.completed.len(), 1);
        let (_n, task_id) = &report.completed[0];
        let t = bridge.board().get(*task_id).await.expect("task");
        assert_eq!(t.status, TaskStatus::Done);
    }
}

//! Integration test: multi-agent orchestration runs an A→B plan and proves
//! that B is only assigned to a worker once A is in the
//! [`TaskStatus::Done`] state.
//!
//! This test builds [`FamilyBridge`] only through its public interface,
//! registers two agents with capabilities, and runs a two-node plan
//! ([`Orchestrator::run`]). Respecting the dependency gate (`B.deps = [A]`)
//! is verified in two ways:
//!
//! 1. **Event order.** `orchestration.step_assigned` events are published
//!    for A before B — i.e. B is activated after A.
//! 2. **Final state.** Both nodes end up in the `Done` state, and the
//!    report's order is `[A, B]`.

use familyclaw_bridge::{
    AgentInfo, AgentRole, FamilyBridge, HostKind, NodeId, OrchestrationPlan, Orchestrator,
    TaskNode, TaskStatus, STEP_ASSIGNED,
};
use familyclaw_core::ids::AgentId;
use familyclaw_core::time;

/// Builds and registers an online agent with the given capabilities at time `now`.
async fn online_agent(
    bridge: &FamilyBridge,
    id: AgentId,
    name: &str,
    role: AgentRole,
    caps: &[&str],
    now: familyclaw_core::time::Timestamp,
) {
    let info =
        AgentInfo::new(id, name, role, HostKind::Local).with_capabilities(caps.iter().copied());
    bridge.register_agent(info).await.expect("register agent");
    bridge.heartbeat(id, now).await.expect("heartbeat");
}

#[tokio::test]
async fn two_node_plan_assigns_b_only_after_a_is_done() {
    let bridge = FamilyBridge::new();
    let now = time::from_unix_secs(1_700_000_000).expect("valid timestamp");

    // Two agents, fixed ids for determinism. Both have sufficient
    // capabilities; nodes are still pinned to roles so the selection is unambiguous.
    let strategist = AgentId::from_uuid(uuid::Uuid::from_u128(0xA));
    let executor = AgentId::from_uuid(uuid::Uuid::from_u128(0xB));
    online_agent(
        &bridge,
        strategist,
        "strategist",
        AgentRole::Strategy,
        &["plan"],
        now,
    )
    .await;
    online_agent(
        &bridge,
        executor,
        "executor",
        AgentRole::Executor,
        &["plan", "system.run"],
        now,
    )
    .await;

    // Subscribe to the event bus BEFORE running, so we see the step_assigned order.
    let mut events = bridge.subscribe();

    // Suunnitelma: A (strategia) → B (suoritus), B riippuu A:sta ja vaatii
    // kyvyn jota vain executorilla on.
    let plan = OrchestrationPlan::new(
        "design_then_build",
        vec![
            TaskNode::new("A", "design the seed", "lay out the architecture")
                .with_role(AgentRole::Strategy)
                .with_capabilities(["plan"]),
            TaskNode::new("B", "build the seed", "implement and ship")
                .with_role(AgentRole::Executor)
                .with_capabilities(["system.run"])
                .with_deps(["A"]),
        ],
    );

    let orch = Orchestrator::new(bridge.clone());
    let report = orch.run(&plan, now).await.expect("orchestration run");

    // --- Final state: both complete, order A then B. ---------------
    assert_eq!(report.completed.len(), 2, "both nodes must complete");
    assert_eq!(report.completed[0].0, NodeId::new("A"), "A runs first");
    assert_eq!(report.completed[1].0, NodeId::new("B"), "B runs second");

    let (a_node, a_task) = &report.completed[0];
    let (b_node, b_task) = &report.completed[1];
    assert_eq!(a_node, &NodeId::new("A"));
    assert_eq!(b_node, &NodeId::new("B"));

    // A osoitettiin strategille ja on Done; B osoitettiin executorille ja on Done.
    let a = bridge.board().get(*a_task).await.expect("A task exists");
    let b = bridge.board().get(*b_task).await.expect("B task exists");
    assert_eq!(a.status, TaskStatus::Done);
    assert_eq!(b.status, TaskStatus::Done);
    assert_eq!(a.assignee, Some(strategist), "A assigned to the strategist");
    assert_eq!(b.assignee, Some(executor), "B assigned to the executor");

    // B luotiin vasta A:n valmistuttua → B:n created_at >= A:n updated_at (Done).
    assert!(
        b.created_at >= a.updated_at,
        "B must be created only after A reached Done"
    );

    // --- Event order: step_assigned for A before B. ---------------
    let mut assigned_order: Vec<AgentId> = Vec::new();
    while let Ok(Some(ev)) = events.try_recv() {
        if let familyclaw_bridge::EventKind::Custom(name) = &ev.kind {
            if name == STEP_ASSIGNED {
                if let Some(src) = ev.source {
                    assigned_order.push(src);
                }
            }
        }
    }
    assert_eq!(
        assigned_order,
        vec![strategist, executor],
        "A (strategist) must be assigned before B (executor)"
    );
}

#[tokio::test]
async fn dependent_node_blocks_when_predecessor_has_no_worker() {
    // If there is no eligible worker for A, the entire run fails and B is
    // never assigned — the dependency chain does not advance past the stub.
    let bridge = FamilyBridge::new();
    let now = time::from_unix_secs(1_700_000_000).expect("valid timestamp");

    // Only executor is online; A requires the Strategy role → no worker for A.
    let executor = AgentId::from_uuid(uuid::Uuid::from_u128(0xB));
    online_agent(
        &bridge,
        executor,
        "executor",
        AgentRole::Executor,
        &["system.run"],
        now,
    )
    .await;

    let plan = OrchestrationPlan::new(
        "blocked",
        vec![
            TaskNode::new("A", "design", "").with_role(AgentRole::Strategy),
            TaskNode::new("B", "build", "")
                .with_role(AgentRole::Executor)
                .with_deps(["A"]),
        ],
    );

    let orch = Orchestrator::new(bridge.clone());
    let err = orch
        .run(&plan, now)
        .await
        .expect_err("A has no eligible worker");
    assert!(matches!(err, familyclaw_core::FamilyClawError::NotFound(_)));

    // B was never assigned: the only online agent has no tasks.
    let executor_tasks = bridge.board().list_for_assignee(executor).await;
    assert!(
        executor_tasks.is_empty(),
        "B must not be assigned when its dependency A could not run"
    );
}

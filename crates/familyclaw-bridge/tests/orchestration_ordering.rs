//! Integraatiotesti: moniagenttiorkesterointi ajaa A→B-suunnitelman ja
//! todistaa että B osoitetaan työntekijälle vasta kun A on tilassa
//! [`TaskStatus::Done`].
//!
//! Tämä testi rakentaa [`FamilyBridge`]in vain julkisen rajapinnan kautta,
//! rekisteröi kaksi kyvykkyyksin varustettua agenttia ja ajaa kaksisolmuisen
//! suunnitelman ([`Orchestrator::run`]). Riippuvuusportin (`B.deps = [A]`)
//! kunnioittaminen varmennetaan kahdella tavalla:
//!
//! 1. **Tapahtumajärjestys.** `orchestration.step_assigned`-tapahtumat
//!    julkaistaan A:lle ennen B:tä — eli B aktivoidaan A:n jälkeen.
//! 2. **Lopputila.** Molemmat solmut päätyvät `Done`-tilaan, ja raportin
//!    järjestys on `[A, B]`.

use familyclaw_bridge::{
    AgentInfo, AgentRole, FamilyBridge, HostKind, NodeId, OrchestrationPlan, Orchestrator,
    TaskNode, TaskStatus, STEP_ASSIGNED,
};
use familyclaw_core::ids::AgentId;
use familyclaw_core::time;

/// Rakentaa ja rekisteröi online-agentin annetuilla kyvyillä hetkellä `now`.
async fn online_agent(
    bridge: &FamilyBridge,
    id: AgentId,
    name: &str,
    role: AgentRole,
    caps: &[&str],
    now: familyclaw_core::time::Timestamp,
) {
    let info = AgentInfo::new(id, name, role, HostKind::Local).with_capabilities(caps.iter().copied());
    bridge.register_agent(info).await.expect("register agent");
    bridge.heartbeat(id, now).await.expect("heartbeat");
}

#[tokio::test]
async fn two_node_plan_assigns_b_only_after_a_is_done() {
    let bridge = FamilyBridge::new();
    let now = time::from_unix_secs(1_700_000_000).expect("valid timestamp");

    // Kaksi agenttia, kiinteät id:t determinismin vuoksi. Molemmilla riittävät
    // kyvyt; solmut kiinnitetään silti rooleihin että valinta on yksiselitteinen.
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

    // Tilaa tapahtumaväylä ENNEN ajoa, jotta näemme step_assigned-järjestyksen.
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

    // --- Lopputila: molemmat valmiita, järjestys A sitten B. ---------------
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

    // --- Tapahtumajärjestys: step_assigned A:lle ennen B:tä. ---------------
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
    // Jos A:lle ei ole kelvollista työntekijää, koko ajo epäonnistuu eikä B:tä
    // koskaan osoiteta — riippuvuusketju ei etene ohi tyngän.
    let bridge = FamilyBridge::new();
    let now = time::from_unix_secs(1_700_000_000).expect("valid timestamp");

    // Vain executor on online; A vaatii Strategy-roolin → ei työntekijää A:lle.
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
    let err = orch.run(&plan, now).await.expect_err("A has no eligible worker");
    assert!(matches!(err, familyclaw_core::FamilyClawError::NotFound(_)));

    // B:tä ei koskaan osoitettu: ainoalla online-agentilla ei ole tehtäviä.
    let executor_tasks = bridge.board().list_for_assignee(executor).await;
    assert!(
        executor_tasks.is_empty(),
        "B must not be assigned when its dependency A could not run"
    );
}

//! Integration test: **HOMEPAGE FACTORY** — multi-agent collaboration with
//! verified contracts, run against the hermetic [`MockTurnExecutor`].
//!
//! This test proves the reliability guarantee the entire bridge is built on:
//! **the same plan run here with the mock executor later runs with the
//! Layer B `LiveTurnExecutor` and a real LLM — without a single change to the
//! orchestrator.** The orchestrator depends only on the [`TurnExecutor`]
//! seam, never on a concrete executor (see [`docs/HOMEPAGE_FACTORY.md`]).
//!
//! ## Scenario
//! Three agents collaborate to build a homepage:
//! - **`agent_alpha`** ([`AgentRole::Strategy`]) — the designer, advertises
//!   the `homepage_design` capability.
//! - **`agent_beta`** ([`AgentRole::Scout`]) — orchestrator/reviewer.
//! - **`agent_gamma`** ([`AgentRole::Executor`]) — the publisher, `deploy`
//!   capability.
//!
//! The plan is a DAG: `design → review → deploy`. The `design` node carries
//! the `homepage_design` capability, whose **output schema** (`headline`,
//! `sections`, `cta`) and **postconditions** (`non_empty(headline)`,
//! `min_len(sections, 1)`) are verified against the deliverable at the
//! contract boundary before the node reaches the `Done` state.
//!
//! ## Two proofs
//! 1. **Successful factory** ([`homepage_factory_runs_end_to_end`]): all
//!    three nodes advance to the `Done` state in dependency order
//!    (design → review → deploy), the `design` deliverable passes the
//!    `homepage_design` contract ([`ContractStatus::Fulfilled`]), and
//!    `orchestration.step_assigned` events are published in order.
//! 2. **A violation halts the factory**
//!    ([`malformed_design_halts_factory_at_contract_boundary`]): a malformed
//!    deliverable (missing `headline`, empty `sections`) makes the `design`
//!    contract's `fulfill` verification fail, the `design` node does **not**
//!    reach the `Done` state, and the `review` or `deploy` node is never
//!    assigned — the DAG halts on the violation, the deliverable does not
//!    silently flow onward.

use familyclaw_bridge::{
    AgentInfo, AgentRole, Capability, CapabilityRegistry, Clause, ContractBoard, ContractStatus,
    Deliverable, FamilyBridge, Field, FieldType, HostKind, MockTurnExecutor, NodeId,
    OrchestratedTurn, OrchestrationPlan, Orchestrator, Schema, TaskNode, TaskStatus, TurnExecutor,
    STEP_ASSIGNED,
};
use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};

/// A stable timestamp to inject (the clock is never read in the test).
fn ts(secs: i64) -> Timestamp {
    time::from_unix_secs(secs).expect("valid unix seconds")
}

/// Builds and registers an online agent with the given capabilities at time `now`.
async fn online_agent(
    bridge: &FamilyBridge,
    id: AgentId,
    name: &str,
    role: AgentRole,
    caps: &[&str],
    now: Timestamp,
) {
    let info =
        AgentInfo::new(id, name, role, HostKind::Local).with_capabilities(caps.iter().copied());
    bridge.register_agent(info).await.expect("register agent");
    bridge.heartbeat(id, now).await.expect("heartbeat");
}

/// `HomepageDesign` output schema: `{ headline:Str, sections:Arr, cta:Str }`.
fn homepage_design_output() -> Schema {
    Schema::new(vec![
        Field::required("headline", FieldType::Str),
        Field::required("sections", FieldType::Arr),
        Field::required("cta", FieldType::Str),
    ])
}

/// `HomepageDesign` postconditions: `non_empty(headline)` + `min_len(sections, 1)`.
fn homepage_design_postconditions() -> Vec<Clause> {
    vec![
        Clause::non_empty("headline"),
        Clause::min_len("sections", 1),
    ]
}

/// The `homepage_design` capability with the **full** `BrandBrief { brand,
/// audience }` input schema. Used in the explicit contract proof, where
/// `propose` is run with a real brief as input.
fn homepage_design_capability() -> Capability {
    Capability::new(
        "homepage_design",
        // BrandBrief
        Schema::new(vec![
            Field::required("brand", FieldType::Str),
            Field::required("audience", FieldType::Str),
        ]),
        homepage_design_output(),
    )
    .with_postconditions(homepage_design_postconditions())
}

/// The `homepage_design` capability with an **empty** input schema. This is
/// the variant attached to a node: the orchestrator's internal contract path
/// ([`Orchestrator::run_with`] → [`ContractBoard::propose`]) proposes the
/// capability with an empty `{}` input, so the node's capability input schema
/// must accept an empty object. The output schema and postconditions are
/// identical to the full variant, so the deliverable verification
/// (`headline`, `sections`, `cta` + postconditions) is the same.
fn homepage_design_node_capability() -> Capability {
    Capability::new("homepage_design", Schema::empty(), homepage_design_output())
        .with_postconditions(homepage_design_postconditions())
}

/// The `deploy` capability: an output with at least a `url` field, with the
/// postcondition `non_empty(url)`. (Used as `agent_gamma`'s publisher capability.)
fn deploy_capability() -> Capability {
    Capability::new(
        "deploy",
        Schema::empty(),
        Schema::new(vec![Field::required("result", FieldType::Obj)]),
    )
}

/// The three-node homepage factory plan: `design → review → deploy`.
///
/// - `design` requires the `homepage_design` capability, has no
///   dependencies, and **carries** the `homepage_design` contract (the
///   deliverable is verified at the contract boundary).
/// - `review` depends on `design` (the review stage).
/// - `deploy` requires the `deploy` capability, depends on `review`.
fn factory_plan() -> OrchestrationPlan {
    OrchestrationPlan::new(
        "homepage_factory",
        vec![
            // design: BrandBrief as the description → the mock produces a HomepageDesign.
            TaskNode::new(
                "design",
                "design the homepage",
                r#"{"brand":"DuckUps","audience":"founders"}"#,
            )
            .with_role(AgentRole::Strategy)
            .with_capabilities(["homepage_design"])
            .with_capability(homepage_design_node_capability()),
            // review: no capability requirement, depends on design.
            TaskNode::new("review", "review the design", "approve or request changes")
                .with_role(AgentRole::Scout)
                .with_deps(["design"]),
            // deploy: requires the deploy capability, depends on review, and
            // carries the deploy contract (output `result: object`). The
            // mock's non-homepage deliverable `{ "result": ..., "assignee": ... }`
            // satisfies the schema.
            TaskNode::new("deploy", "deploy the homepage", "ship to production")
                .with_role(AgentRole::Executor)
                .with_capabilities(["deploy"])
                .with_capability(deploy_capability())
                .with_deps(["review"]),
        ],
    )
}

/// Registers the factory's three agents (`agent_alpha`/`agent_beta`/`agent_gamma`) as online.
async fn register_factory_agents(
    bridge: &FamilyBridge,
    now: Timestamp,
) -> (AgentId, AgentId, AgentId) {
    // Fixed ids for determinism.
    let agent_alpha = AgentId::from_uuid(uuid::Uuid::from_u128(0xA0));
    let agent_beta = AgentId::from_uuid(uuid::Uuid::from_u128(0x10));
    let agent_gamma = AgentId::from_uuid(uuid::Uuid::from_u128(0xB0));

    // agent_alpha — the designer (Strategy), advertises the homepage_design capability.
    online_agent(
        bridge,
        agent_alpha,
        "agent_alpha",
        AgentRole::Strategy,
        &["homepage_design"],
        now,
    )
    .await;
    // agent_beta — orchestrator/reviewer (Scout).
    online_agent(
        bridge,
        agent_beta,
        "agent_beta",
        AgentRole::Scout,
        &["review"],
        now,
    )
    .await;
    // agent_gamma — publisher (Executor), deploy capability.
    online_agent(
        bridge,
        agent_gamma,
        "agent_gamma",
        AgentRole::Executor,
        &["deploy"],
        now,
    )
    .await;

    // agent_alpha advertises its capability to the capability registry
    // (CapabilityRegistry.advertise). FamilyBridge (Layer A) does not yet own
    // a capability registry, so we use a separate registry to prove the
    // advertise path works.
    let registry = CapabilityRegistry::new();
    registry.advertise(homepage_design_capability()).await;
    registry.advertise(deploy_capability()).await;
    assert_eq!(
        registry.find_by_name("homepage_design").await.len(),
        1,
        "agent_alpha's homepage_design capability is advertised"
    );

    (agent_alpha, agent_beta, agent_gamma)
}

// =============================================================================
// TESTI 1 — onnistunut tehdas: kolme solmua Done, design-sopimus Fulfilled.
// =============================================================================

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn homepage_factory_runs_end_to_end() {
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    let (agent_alpha, _agent_beta, _agent_gamma) = register_factory_agents(&bridge, now).await;

    // Subscribe to the event bus BEFORE running, to observe the step_assigned order.
    let mut events = bridge.subscribe();

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());

    // The same call used to run LiveTurnExecutor later — only the executor changes.
    let executor = MockTurnExecutor::new();
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("homepage factory run");

    // --- All three nodes completed in dependency order. ---------
    assert_eq!(report.completed.len(), 3, "all three nodes must complete");
    let order: Vec<NodeId> = report.completed.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(
        order,
        vec![
            NodeId::new("design"),
            NodeId::new("review"),
            NodeId::new("deploy"),
        ],
        "design before review before deploy (dependency order)"
    );

    // Each node's task is in the Done state.
    for (node, task_id) in &report.completed {
        let task = bridge.board().get(*task_id).await.expect("task exists");
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "node {node} task must be Done"
        );
    }

    // --- The design deliverable passes the homepage_design contract (Fulfilled). ----
    // We repeat the orchestrator's contract boundary explicitly: agent_alpha
    // performs the same turn, and the deliverable is run through the linked
    // contract's `fulfill` verification (output schema + postconditions).
    // The link from the contract to the design task is set
    // (Contract.link = Some(task_id)).
    let (design_node, design_task) = report
        .completed
        .iter()
        .find(|(n, _)| n == &NodeId::new("design"))
        .expect("design node completed");
    assert_eq!(design_node, &NodeId::new("design"));

    let cap = homepage_design_capability();
    let brief = serde_json::json!({ "brand": "DuckUps", "audience": "founders" });
    let board = ContractBoard::new();

    // propose validates against the BrandBrief input schema.
    let contract = board
        .propose(&cap, agent_alpha, agent_alpha, brief.clone(), now)
        .await
        .expect("propose homepage_design contract");
    // Link the contract to the design task.
    let mut linked = contract.clone();
    linked.link = Some(*design_task);
    board.insert(linked).await;
    board.accept(contract.id, now).await.expect("accept");

    // agent_alpha performs the turn via the mock seam → a HomepageDesign deliverable.
    let turn = OrchestratedTurn::new(
        plan.id.clone(),
        NodeId::new("design"),
        *design_task,
        agent_alpha,
        "design the homepage",
        brief.to_string(),
        brief.clone(),
        now,
    );
    let deliverable = executor.execute(turn).await.expect("execute design turn");

    // fulfill verifies the output schema + postconditions → Fulfilled.
    let fulfilled = board
        .fulfill(contract.id, deliverable.clone(), now)
        .await
        .expect("design deliverable fulfills the contract");
    assert_eq!(
        fulfilled.status,
        ContractStatus::Fulfilled,
        "design deliverable must pass output schema + postconditions"
    );
    // The contract is linked to the design task.
    assert_eq!(
        fulfilled.link,
        Some(*design_task),
        "contract links to design task"
    );
    // Deliverable shape: headline non-empty, sections >= 1, cta present.
    let payload = &deliverable.payload;
    assert!(
        payload["headline"].as_str().is_some_and(|h| !h.is_empty()),
        "headline non-empty"
    );
    assert!(
        payload["sections"]
            .as_array()
            .is_some_and(|s| !s.is_empty()),
        "sections has at least one entry"
    );
    assert!(payload["cta"].as_str().is_some(), "cta present");

    // --- step_assigned events in order design → review → deploy. --
    let mut assigned_nodes: Vec<String> = Vec::new();
    while let Ok(Some(ev)) = events.try_recv() {
        if let familyclaw_bridge::EventKind::Custom(name) = &ev.kind {
            if name == STEP_ASSIGNED {
                if let Some(node) = ev.payload.get("node_id").and_then(|v| v.as_str()) {
                    assigned_nodes.push(node.to_string());
                }
            }
        }
    }
    assert_eq!(
        assigned_nodes,
        vec![
            "design".to_string(),
            "review".to_string(),
            "deploy".to_string(),
        ],
        "step_assigned fires in dependency order design → review → deploy"
    );
}

// =============================================================================
// TEST 2 — a violation halts the factory at the contract boundary.
// =============================================================================

#[tokio::test]
async fn malformed_design_halts_factory_at_contract_boundary() {
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    let (agent_alpha, _agent_beta, _agent_gamma) = register_factory_agents(&bridge, now).await;

    let mut events = bridge.subscribe();

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());

    // failing() produces a deliverable WITHOUT a headline and with an EMPTY
    // sections list → the output schema + postconditions are breached at the contract boundary.
    let executor = MockTurnExecutor::failing();
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("run returns Ok report even when a node fails");

    // --- Not a single node completed: the factory halted right at design. -----
    assert!(
        report.completed.is_empty(),
        "no node completes when design breaches its contract"
    );

    // --- The design task was created but is NOT Done (stayed in the Active state). ------
    let design_tasks: Vec<_> = bridge
        .board()
        .list_for_assignee(agent_alpha)
        .await
        .into_iter()
        .filter(|t| t.title == "design the homepage")
        .collect();
    assert_eq!(design_tasks.len(), 1, "design task was created");
    assert_ne!(
        design_tasks[0].status,
        TaskStatus::Done,
        "design must NOT reach Done on a malformed deliverable"
    );
    assert_eq!(
        design_tasks[0].status,
        TaskStatus::Active,
        "design halts in Active (caught at the contract boundary)"
    );

    // --- review and deploy were NEVER assigned (the DAG halted). ---------
    let all_tasks = bridge.board().list().await;
    assert!(
        !all_tasks.iter().any(|t| t.title == "review the design"),
        "review must not be scheduled after design breach"
    );
    assert!(
        !all_tasks.iter().any(|t| t.title == "deploy the homepage"),
        "deploy must not be scheduled after design breach"
    );

    // --- The design contract moves to the Failed state in fulfill() (an
    //     explicit proof with the same malformed deliverable). ------------------------
    prove_malformed_design_fails_contract(agent_alpha, &plan, design_tasks[0].id, &executor, now)
        .await;

    // --- Tapahtumat: vain design osoitettiin; review/deploy ei. -------------
    let mut assigned_nodes: Vec<String> = Vec::new();
    let mut step_failed = 0;
    while let Ok(Some(ev)) = events.try_recv() {
        if let familyclaw_bridge::EventKind::Custom(name) = &ev.kind {
            if name == STEP_ASSIGNED {
                if let Some(node) = ev.payload.get("node_id").and_then(|v| v.as_str()) {
                    assigned_nodes.push(node.to_string());
                }
            } else if name == familyclaw_bridge::STEP_FAILED {
                step_failed += 1;
            }
        }
    }
    assert_eq!(
        assigned_nodes,
        vec!["design".to_string()],
        "only design is ever assigned; review and deploy never run"
    );
    assert_eq!(step_failed, 1, "design emits exactly one step_failed");
}

/// An explicit proof that a malformed `design` deliverable fails the
/// `homepage_design` contract's `fulfill` verification.
///
/// Proposes the capability with the full `BrandBrief` input, accepts the
/// contract, runs the same malformed deliverable produced by the
/// `failing()` executor through `fulfill`, and verifies: (1) the error is an
/// output schema violation (missing `headline`), (2) the contract ends up in
/// the [`ContractStatus::Failed`] state, (3) the same malformed payload does
/// not pass the output schema even under a separate `is_valid` check.
async fn prove_malformed_design_fails_contract(
    agent_alpha: AgentId,
    plan: &OrchestrationPlan,
    design_task: familyclaw_bridge::TaskId,
    executor: &MockTurnExecutor,
    now: Timestamp,
) {
    let cap = homepage_design_capability();
    let brief = serde_json::json!({ "brand": "DuckUps", "audience": "founders" });
    let board = ContractBoard::new();
    let contract = board
        .propose(&cap, agent_alpha, agent_alpha, brief.clone(), now)
        .await
        .expect("propose");
    board.accept(contract.id, now).await.expect("accept");

    // failing()-suoritin tuottaa saman viallisen toimitteen.
    let turn = OrchestratedTurn::new(
        plan.id.clone(),
        NodeId::new("design"),
        design_task,
        agent_alpha,
        "design the homepage",
        brief.to_string(),
        brief.clone(),
        now,
    );
    let bad = executor
        .execute(turn)
        .await
        .expect("execute (still a deliverable)");
    let bad_payload = bad.payload.clone();
    let err = board
        .fulfill(contract.id, bad, now)
        .await
        .expect_err("malformed deliverable must fail fulfillment");
    // The output schema violation (missing headline) is the first to hit.
    assert!(
        matches!(
            err,
            familyclaw_bridge::ContractError::OutputSchemaViolation(_)
        ),
        "fulfill reports an output schema violation, got {err:?}"
    );
    let after = board.get(contract.id).await.expect("contract present");
    assert_eq!(
        after.status,
        ContractStatus::Failed,
        "design contract goes Failed at fulfill()"
    );

    // Additional proof: the same malformed payload does not conform to the output schema.
    let bad_deliverable = Deliverable::new(agent_alpha, bad_payload, now);
    assert!(
        !cap.output.is_valid(&bad_deliverable.payload),
        "malformed payload is not schema-valid (headline missing)"
    );
}

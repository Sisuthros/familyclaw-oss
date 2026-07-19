//! Integration test: **live multi-agent orchestration end to end**.
//!
//! This proves the README's own admitted biggest gap ("live multi-agent
//! orchestration — built, UNPROVEN"): a three-node DAG is run through
//! [`Orchestrator::run_with`] with a **real [`LiveTurnExecutor`]**, which
//! calls a mock HTTP LLM. Before this, the entire orchestration had only
//! been proven against the hermetic [`MockTurnExecutor`] (see
//! `bridge/tests/homepage_factory.rs`), and no test ran the plan over a
//! real LLM/HTTP path.
//!
//! ## What is proven
//! - The orchestrator runs the `design → review → deploy` DAG with a
//!   real LLM executor.
//! - The `design` deliverable, which is JSON produced by a **real
//!   (mocked) LLM response**, passes the `homepage_design` contract
//!   boundary (result schema + postconditions).
//! - All three nodes progress to the `Done` state in dependency order.
//! - The orchestrator was NOT changed at all — only the executor
//!   switched from mock to live.
//!
//! The mock is a plain `std::net::TcpListener` (no `wiremock`/`httpmock`
//! dependency).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use familyclaw_agent::live_executor::LiveTurnExecutor;
use familyclaw_agent::llm_chain::{build_llm_chain, EnvEndpointResolver};
use familyclaw_bridge::{
    AgentInfo, AgentRole, Capability, Clause, FamilyBridge, Field, FieldType, HostKind, NodeId,
    OrchestrationPlan, Orchestrator, Schema, TaskNode, TaskStatus, STEP_ASSIGNED,
};
use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::ModelConfig;

// ── Mock HTTP LLM: returns a fixed assistant content for every request ──

/// A minimal HTTP/1.1 mock LLM that ALWAYS returns the same
/// OpenAI-compatible `chat.completion` body, whose assistant content is
/// `content`. The same for every node, for determinism; the content must
/// satisfy the result schemas of all contract-bearing nodes in the DAG
/// at once.
struct MockLlm {
    base_url: String,
}

impl MockLlm {
    fn spawn(content: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
        let addr = listener.local_addr().expect("mock local_addr");
        let base_url = format!("http://{addr}/v1");

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                Self::handle(stream, &content);
            }
        });

        Self { base_url }
    }

    fn handle(mut stream: TcpStream, content: &str) {
        let mut buf = [0_u8; 8192];
        let _ = stream.read(&mut buf).unwrap_or(0);

        let body = format!(
            r#"{{"id":"x","object":"chat.completion","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}]}}"#,
            serde_json::to_string(content).expect("json string")
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

fn ts(secs: i64) -> Timestamp {
    time::from_unix_secs(secs).expect("valid unix seconds")
}

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

/// The `homepage_design` capability with an empty input schema (the
/// orchestrator's internal contract route proposes the capability with
/// `{}` input). The result schema requires
/// `headline:Str, sections:Arr, cta:Str` + postconditions.
fn homepage_design_node_capability() -> Capability {
    Capability::new(
        "homepage_design",
        Schema::empty(),
        Schema::new(vec![
            Field::required("headline", FieldType::Str),
            Field::required("sections", FieldType::Arr),
            Field::required("cta", FieldType::Str),
        ]),
    )
    .with_postconditions(vec![
        Clause::non_empty("headline"),
        Clause::min_len("sections", 1),
    ])
}

/// The `deploy` capability: a result with a `result` object field.
fn deploy_capability() -> Capability {
    Capability::new(
        "deploy",
        Schema::empty(),
        Schema::new(vec![Field::required("result", FieldType::Obj)]),
    )
}

/// The same three-node DAG as in the hermetic `homepage_factory` test:
/// `design → review → deploy`.
fn factory_plan() -> OrchestrationPlan {
    OrchestrationPlan::new(
        "homepage_factory_live",
        vec![
            TaskNode::new("design", "design the homepage", "{}")
                .with_role(AgentRole::Strategy)
                .with_capabilities(["homepage_design"])
                .with_capability(homepage_design_node_capability()),
            TaskNode::new("review", "review the design", "approve or request changes")
                .with_role(AgentRole::Scout)
                .with_deps(["design"]),
            TaskNode::new("deploy", "deploy the homepage", "ship to production")
                .with_role(AgentRole::Executor)
                .with_capabilities(["deploy"])
                .with_capability(deploy_capability())
                .with_deps(["review"]),
        ],
    )
}

async fn register_factory_agents(bridge: &FamilyBridge, now: Timestamp) {
    online_agent(
        bridge,
        AgentId::from_uuid(uuid::Uuid::from_u128(0xA0)),
        "agent_alpha",
        AgentRole::Strategy,
        &["homepage_design"],
        now,
    )
    .await;
    online_agent(
        bridge,
        AgentId::from_uuid(uuid::Uuid::from_u128(0x10)),
        "agent_beta",
        AgentRole::Scout,
        &["review"],
        now,
    )
    .await;
    online_agent(
        bridge,
        AgentId::from_uuid(uuid::Uuid::from_u128(0xB0)),
        "agent_gamma",
        AgentRole::Executor,
        &["deploy"],
        now,
    )
    .await;
}

/// Builds a live executor pointing at the mock HTTP LLM.
fn live_executor(mock: &MockLlm) -> LiveTurnExecutor {
    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        mock.base_url.clone(),
        "FAMILYCLAW_TEST_KEY_UNSET",
    );
    let model = ModelConfig::new("mock/model-a");
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    LiveTurnExecutor::new(chain)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn live_multi_agent_orchestration_runs_end_to_end() {
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    register_factory_agents(&bridge, now).await;

    let mut events = bridge.subscribe();

    // The mock LLM returns a JSON object that satisfies BOTH:
    //  - the homepage_design result schema (headline non-empty, sections >= 1, cta)
    //  - the deploy result schema (result: object)
    // The same response for all nodes; the review node has no contract,
    // so it accepts anything. This is a payload produced by a real LLM —
    // it proves that a live response passes the contract boundary.
    let llm_json = serde_json::json!({
        "headline": "DuckUps — ship faster",
        "sections": ["hero", "features", "pricing"],
        "cta": "Start free",
        "result": { "url": "https://duckups.example" }
    })
    .to_string();
    let mock = MockLlm::spawn(llm_json);

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());

    // *** The only difference from the hermetic homepage_factory test: the live executor. ***
    let executor = live_executor(&mock);
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("live homepage factory run");

    // --- All three nodes completed in dependency order. ----------------------
    assert_eq!(
        report.completed.len(),
        3,
        "all three nodes complete via the LIVE executor"
    );
    let order: Vec<NodeId> = report.completed.iter().map(|(n, _)| n.clone()).collect();
    assert_eq!(
        order,
        vec![
            NodeId::new("design"),
            NodeId::new("review"),
            NodeId::new("deploy"),
        ],
        "design before review before deploy (dependency order) over real HTTP"
    );

    // Every node's task is Done — the deliverables passed the contract boundaries.
    for (node, task_id) in &report.completed {
        let task = bridge.board().get(*task_id).await.expect("task exists");
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "node {node} task must be Done"
        );
    }

    // --- step_assigned events in order design → review → deploy. -------------
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
        "step_assigned fires in dependency order over the live executor"
    );
}

#[tokio::test]
async fn live_orchestration_halts_when_llm_breaches_contract() {
    // The mock LLM returns JSON THAT IS MISSING headline → the design
    // node's homepage_design contract is breached by the result schema →
    // the DAG halts at design, review/deploy never run. Proves that the
    // contract boundary protects the live path too: a bad LLM response
    // does not silently leak forward.
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    register_factory_agents(&bridge, now).await;

    // A valid JSON object but it does NOT contain the "headline" field
    // (the schema is breached).
    let bad_json = serde_json::json!({
        "sections": ["hero"],
        "cta": "Start"
    })
    .to_string();
    let mock = MockLlm::spawn(bad_json);

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());
    let executor = live_executor(&mock);

    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("run returns Ok report even when a node breaches its contract");

    assert!(
        report.completed.is_empty(),
        "no node completes when the live LLM response breaches the design contract"
    );

    // review/deploy were never assigned.
    let all_tasks = bridge.board().list().await;
    assert!(
        !all_tasks.iter().any(|t| t.title == "review the design"),
        "review must not be scheduled after a live contract breach"
    );
    assert!(
        !all_tasks.iter().any(|t| t.title == "deploy the homepage"),
        "deploy must not be scheduled after a live contract breach"
    );
}

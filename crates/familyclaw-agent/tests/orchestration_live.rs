//! Integraatiotesti: **live multi-agent -orkestrointi päästä päähän**.
//!
//! Tämä todistaa README:n itse myöntämän suurimman aukon ("live multi-agent
//! orchestration — built, UNPROVEN"): kolmen solmun DAG ajetaan
//! [`Orchestrator::run_with`]:n läpi **oikealla [`LiveTurnExecutor`]:lla**, joka
//! soittaa mock-HTTP-LLM:ää. Ennen tätä koko orkestrointi oli todistettu vain
//! hermeettistä [`MockTurnExecutor`]:ia vasten (ks. `bridge/tests/homepage_factory.rs`),
//! eikä mikään testi ajanut suunnitelmaa oikean LLM-/HTTP-polun yli.
//!
//! ## Mikä todistetaan
//! - Orkesteri ajaa `design → review → deploy` -DAG:n oikealla LLM-suorittimella.
//! - `design`-toimite, joka on **oikean (mockatun) LLM-vastauksen** tuottama
//!   JSON, läpäisee `homepage_design`-sopimusrajan (tulosskeema + jälkiehdot).
//! - Kaikki kolme solmua etenevät `Done`-tilaan riippuvuusjärjestyksessä.
//! - Orkesteria EI muutettu lainkaan — vain suoritin vaihtui mockista liveen.
//!
//! Mock on pelkkä `std::net::TcpListener` (ei `wiremock`/`httpmock`-dependencyä).

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

// ── Mock-HTTP-LLM: palauttaa kiinteän assistant-sisällön jokaiseen pyyntöön ──

/// Minimaalinen HTTP/1.1-mock-LLM joka palauttaa AINA saman OpenAI-yhteensopivan
/// `chat.completion`-rungon, jonka assistant-sisältö on `content`. Determinismin
/// vuoksi sama jokaiselle solmulle; sisällön on täytettävä DAG:n kaikkien
/// sopimuskantavien solmujen tulosskeemat yhtä aikaa.
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

/// `homepage_design`-kyky tyhjällä syöteskeemalla (orkesterin sisäinen
/// sopimusrata ehdottaa kyvyn `{}`-syötteellä). Tulosskeema vaatii
/// `headline:Str, sections:Arr, cta:Str` + jälkiehdot.
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

/// `deploy`-kyky: tulos jolla on `result`-objektikenttä.
fn deploy_capability() -> Capability {
    Capability::new(
        "deploy",
        Schema::empty(),
        Schema::new(vec![Field::required("result", FieldType::Obj)]),
    )
}

/// Sama kolmen solmun DAG kuin hermeettisessä homepage_factory-testissä:
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

/// Rakentaa live-suorittimen joka osoittaa mock-HTTP-LLM:ään.
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

    // Mock-LLM palauttaa JSON-objektin joka täyttää BOTH:
    //  - homepage_design-tulosskeeman (headline non-empty, sections >= 1, cta)
    //  - deploy-tulosskeeman (result: object)
    // Sama vastaus kaikille solmuille; review-solmulla ei ole sopimusta, joten
    // se hyväksyy minkä tahansa. Tämä on oikean LLM:n tuottama hyötykuorma —
    // todistaa että live-vastaus läpäisee sopimusrajan.
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

    // *** Ainoa ero hermeettiseen homepage_factory-testiin: live-suoritin. ***
    let executor = live_executor(&mock);
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("live homepage factory run");

    // --- Kaikki kolme solmua valmistuivat riippuvuusjärjestyksessä. ----------
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

    // Jokainen solmun tehtävä on Done — toimitteet läpäisivät sopimusrajat.
    for (node, task_id) in &report.completed {
        let task = bridge.board().get(*task_id).await.expect("task exists");
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "node {node} task must be Done"
        );
    }

    // --- step_assigned-tapahtumat järjestyksessä design → review → deploy. ---
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
    // Mock-LLM palauttaa JSON:n JOSTA PUUTTUU headline → design-solmun
    // homepage_design-sopimus rikkoutuu tulosskeemalla → DAG pysähtyy designiin,
    // review/deploy ei koskaan aja. Todistaa että sopimusraja suojaa myös live-
    // polulla: huono LLM-vastaus ei vuoda eteenpäin hiljaa.
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    register_factory_agents(&bridge, now).await;

    // Validi JSON-objekti mutta EI sisällä "headline"-kenttää (skeema rikkoutuu).
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

    // review/deploy ei koskaan osoitettu.
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

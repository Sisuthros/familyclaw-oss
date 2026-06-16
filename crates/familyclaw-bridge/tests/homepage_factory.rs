//! Integraatiotesti: **HOMEPAGE FACTORY** — moniagenttiyhteistyö todennetuin
//! sopimuksin, ajettuna hermeettistä [`MockTurnExecutor`]-suorittajaa vasten.
//!
//! Tämä testi todistaa luotettavuustakuun jonka varaan koko silta rakentuu:
//! **sama suunnitelma joka ajetaan tässä mock-suorittajalla ajaa myöhemmin
//! **Layer B `LiveTurnExecutor`:lla oikealla LLM:llä — ilman yhtäkään muutosta
//! orkesteriin.** Orkesteri riippuu vain [`TurnExecutor`]-saumasta, ei
//! konkreettisesta suorittajasta (ks. [`docs/HOMEPAGE_FACTORY.md`]).
//!
//! ## Skenaario
//! Kolme agenttia tekevät yhteistyötä etusivun rakentamiseksi:
//! - **`agent_alpha`** ([`AgentRole::Strategy`]) — suunnittelija, mainostaa kykyä
//!   `homepage_design`.
//! - **`agent_beta`** ([`AgentRole::Scout`]) — orkesteroija/katselmoija.
//! - **`agent_gamma`** ([`AgentRole::Executor`]) — julkaisija, kyky `deploy`.
//!
//! Suunnitelma on DAG: `design → review → deploy`. `design`-solmu kantaa
//! `homepage_design`-kyvyn, jonka **tulosskeema** (`headline`, `sections`,
//! `cta`) ja **jälkiehdot** (`non_empty(headline)`, `min_len(sections, 1)`)
//! todennetaan toimitteesta sopimusrajalla ennen kuin solmu pääsee
//! `Done`-tilaan.
//!
//! ## Kaksi todistusta
//! 1. **Onnistunut tehdas** ([`homepage_factory_runs_end_to_end`]): kaikki
//!    kolme solmua etenevät `Done`-tilaan riippuvuusjärjestyksessä
//!    (design → review → deploy), `design`-toimite läpäisee
//!    `homepage_design`-sopimuksen ([`ContractStatus::Fulfilled`]), ja
//!    `orchestration.step_assigned`-tapahtumat julkaistaan järjestyksessä.
//! 2. **Rikkomus pysäyttää tehtaan**
//!    ([`malformed_design_halts_factory_at_contract_boundary`]): viallinen
//!    toimite (puuttuva `headline`, tyhjä `sections`) saa `design`-sopimuksen
//!    `fulfill`-todennuksen kaatumaan, `design`-solmu **ei** pääse
//!    `Done`-tilaan, eikä `review`- tai `deploy`-solmua koskaan osoiteta —
//!    DAG pysähtyy rikkomukseen, toimite ei vuoda eteenpäin hiljaa.

use familyclaw_bridge::{
    AgentInfo, AgentRole, Capability, CapabilityRegistry, Clause, ContractBoard, ContractStatus,
    Deliverable, FamilyBridge, Field, FieldType, HostKind, MockTurnExecutor, NodeId,
    OrchestratedTurn, OrchestrationPlan, Orchestrator, Schema, TaskNode, TaskStatus, TurnExecutor,
    STEP_ASSIGNED,
};
use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};

/// Vakaa aikaleima injektoitavaksi (kelloa ei koskaan lueta testissä).
fn ts(secs: i64) -> Timestamp {
    time::from_unix_secs(secs).expect("valid unix seconds")
}

/// Rakentaa ja rekisteröi online-agentin annetuilla kyvyillä hetkellä `now`.
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

/// `HomepageDesign`-tulosskeema: `{ headline:Str, sections:Arr, cta:Str }`.
fn homepage_design_output() -> Schema {
    Schema::new(vec![
        Field::required("headline", FieldType::Str),
        Field::required("sections", FieldType::Arr),
        Field::required("cta", FieldType::Str),
    ])
}

/// `HomepageDesign`-jälkiehdot: `non_empty(headline)` + `min_len(sections, 1)`.
fn homepage_design_postconditions() -> Vec<Clause> {
    vec![
        Clause::non_empty("headline"),
        Clause::min_len("sections", 1),
    ]
}

/// `homepage_design`-kyky **täydellä** `BrandBrief { brand, audience }`-
/// syöteskeemalla. Käytetään eksplisiittisessä sopimustodistuksessa, jossa
/// `propose` ajetaan oikealla briefillä syötteenä.
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

/// `homepage_design`-kyky **tyhjällä** syöteskeemalla. Tämä on solmuun
/// kiinnitettävä variantti: orkesterin sisäinen sopimusrata ([`Orchestrator::
/// run_with`] → [`ContractBoard::propose`]) ehdottaa kyvyn tyhjällä `{}`-
/// syötteellä, joten solmun kyvyn syöteskeeman on hyväksyttävä tyhjä objekti.
/// Tulosskeema ja jälkiehdot ovat identtiset täyden variantin kanssa, joten
/// toimitteen todennus (`headline`, `sections`, `cta` + jälkiehdot) on sama.
fn homepage_design_node_capability() -> Capability {
    Capability::new("homepage_design", Schema::empty(), homepage_design_output())
        .with_postconditions(homepage_design_postconditions())
}

/// `deploy`-kyky: tulos jolla on vähintään `url`-kenttä, jälkiehto
/// `non_empty(url)`. (Käytetään `agent_gamma`-julkaisijan kykynä.)
fn deploy_capability() -> Capability {
    Capability::new(
        "deploy",
        Schema::empty(),
        Schema::new(vec![Field::required("result", FieldType::Obj)]),
    )
}

/// Kolmen solmun etusivutehtaan suunnitelma: `design → review → deploy`.
///
/// - `design` vaatii kyvyn `homepage_design`, ei riippuvuuksia, ja **kantaa**
///   `homepage_design`-sopimuksen (toimite todennetaan sopimusrajalla).
/// - `review` riippuu `design`:sta (katselmointivaihe).
/// - `deploy` vaatii kyvyn `deploy`, riippuu `review`:sta.
fn factory_plan() -> OrchestrationPlan {
    OrchestrationPlan::new(
        "homepage_factory",
        vec![
            // design: BrandBrief kuvauksena → mock tuottaa HomepageDesignin.
            TaskNode::new(
                "design",
                "design the homepage",
                r#"{"brand":"DuckUps","audience":"founders"}"#,
            )
            .with_role(AgentRole::Strategy)
            .with_capabilities(["homepage_design"])
            .with_capability(homepage_design_node_capability()),
            // review: ei kyvyn vaatimusta, riippuu designista.
            TaskNode::new("review", "review the design", "approve or request changes")
                .with_role(AgentRole::Scout)
                .with_deps(["design"]),
            // deploy: vaatii deploy-kyvyn, riippuu reviewista, ja kantaa
            // deploy-sopimuksen (tulos `result: object`). Mockin ei-homepage-
            // toimite `{ "result": ..., "assignee": ... }` täyttää skeeman.
            TaskNode::new("deploy", "deploy the homepage", "ship to production")
                .with_role(AgentRole::Executor)
                .with_capabilities(["deploy"])
                .with_capability(deploy_capability())
                .with_deps(["review"]),
        ],
    )
}

/// Rekisteröi tehtaan kolme agenttia (`agent_alpha`/`agent_beta`/`agent_gamma`) onlineksi.
async fn register_factory_agents(
    bridge: &FamilyBridge,
    now: Timestamp,
) -> (AgentId, AgentId, AgentId) {
    // Kiinteät id:t determinismin vuoksi.
    let agent_alpha = AgentId::from_uuid(uuid::Uuid::from_u128(0xA0));
    let agent_beta = AgentId::from_uuid(uuid::Uuid::from_u128(0x10));
    let agent_gamma = AgentId::from_uuid(uuid::Uuid::from_u128(0xB0));

    // agent_alpha — suunnittelija (Strategy), mainostaa homepage_design-kyvyn.
    online_agent(
        bridge,
        agent_alpha,
        "agent_alpha",
        AgentRole::Strategy,
        &["homepage_design"],
        now,
    )
    .await;
    // agent_beta — orkesteroija/katselmoija (Scout).
    online_agent(
        bridge,
        agent_beta,
        "agent_beta",
        AgentRole::Scout,
        &["review"],
        now,
    )
    .await;
    // agent_gamma — julkaisija (Executor), kyky deploy.
    online_agent(
        bridge,
        agent_gamma,
        "agent_gamma",
        AgentRole::Executor,
        &["deploy"],
        now,
    )
    .await;

    // agent_alpha mainostaa kykynsä kykyrekisteriin (CapabilityRegistry.advertise).
    // FamilyBridge (KERROS A) ei vielä omista kykyrekisteriä, joten käytämme
    // erillistä rekisteriä todistaaksemme advertise-reitin toimivuuden.
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

    // Tilaa tapahtumaväylä ENNEN ajoa nähdäksemme step_assigned-järjestyksen.
    let mut events = bridge.subscribe();

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());

    // Sama kutsu jolla LiveTurnExecutor ajetaan myöhemmin — vain suoritin vaihtuu.
    let executor = MockTurnExecutor::new();
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("homepage factory run");

    // --- Kaikki kolme solmua valmistuivat riippuvuusjärjestyksessä. ---------
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

    // Jokainen solmun tehtävä on Done-tilassa.
    for (node, task_id) in &report.completed {
        let task = bridge.board().get(*task_id).await.expect("task exists");
        assert_eq!(
            task.status,
            TaskStatus::Done,
            "node {node} task must be Done"
        );
    }

    // --- design-toimite läpäisee homepage_design-sopimuksen (Fulfilled). ----
    // Toistamme orkesterin sopimusrajan eksplisiittisesti: agent_alpha suorittaa
    // saman vuoron, ja toimite ajetaan linkitetyn sopimuksen `fulfill`-
    // todennuksen läpi (tulosskeema + jälkiehdot). Linkki sopimuksesta
    // design-tehtävään asetetaan (Contract.link = Some(task_id)).
    let (design_node, design_task) = report
        .completed
        .iter()
        .find(|(n, _)| n == &NodeId::new("design"))
        .expect("design node completed");
    assert_eq!(design_node, &NodeId::new("design"));

    let cap = homepage_design_capability();
    let brief = serde_json::json!({ "brand": "DuckUps", "audience": "founders" });
    let board = ContractBoard::new();

    // propose validoi BrandBrief-syöteskeemaa vasten.
    let contract = board
        .propose(&cap, agent_alpha, agent_alpha, brief.clone(), now)
        .await
        .expect("propose homepage_design contract");
    // Linkitä sopimus design-tehtävään.
    let mut linked = contract.clone();
    linked.link = Some(*design_task);
    board.insert(linked).await;
    board.accept(contract.id, now).await.expect("accept");

    // agent_alpha suorittaa vuoron mock-saumalla → HomepageDesign-toimite.
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

    // fulfill todentaa tulosskeeman + jälkiehdot → Fulfilled.
    let fulfilled = board
        .fulfill(contract.id, deliverable.clone(), now)
        .await
        .expect("design deliverable fulfills the contract");
    assert_eq!(
        fulfilled.status,
        ContractStatus::Fulfilled,
        "design deliverable must pass output schema + postconditions"
    );
    // Sopimus on linkitetty design-tehtävään.
    assert_eq!(
        fulfilled.link,
        Some(*design_task),
        "contract links to design task"
    );
    // Toimitteen muoto: headline ei-tyhjä, sections >= 1, cta läsnä.
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

    // --- step_assigned-tapahtumat järjestyksessä design → review → deploy. --
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
// TESTI 2 — rikkomus pysäyttää tehtaan sopimusrajalla.
// =============================================================================

#[tokio::test]
async fn malformed_design_halts_factory_at_contract_boundary() {
    let bridge = FamilyBridge::new();
    let now = ts(1_700_000_000);
    let (agent_alpha, _agent_beta, _agent_gamma) = register_factory_agents(&bridge, now).await;

    let mut events = bridge.subscribe();

    let plan = factory_plan();
    let orch = Orchestrator::new(bridge.clone());

    // failing() tuottaa toimitteen ILMAN headlinea ja TYHJÄLLÄ sections-listalla
    // → tulosskeema + jälkiehdot rikkoutuvat sopimusrajalla.
    let executor = MockTurnExecutor::failing();
    let report = orch
        .run_with(&plan, now, &executor)
        .await
        .expect("run returns Ok report even when a node fails");

    // --- Yksikään solmu ei valmistunut: tehdas pysähtyi heti designiin. -----
    assert!(
        report.completed.is_empty(),
        "no node completes when design breaches its contract"
    );

    // --- design-tehtävä luotiin mutta EI ole Done (jäi Active-tilaan). ------
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

    // --- review ja deploy EIVÄT koskaan osoittuneet (DAG pysähtyi). ---------
    let all_tasks = bridge.board().list().await;
    assert!(
        !all_tasks.iter().any(|t| t.title == "review the design"),
        "review must not be scheduled after design breach"
    );
    assert!(
        !all_tasks.iter().any(|t| t.title == "deploy the homepage"),
        "deploy must not be scheduled after design breach"
    );

    // --- design-sopimus menee Failed-tilaan fulfill():ssä (eksplisiittinen
    //     todistus samalla viallisella toimitteella). ------------------------
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

/// Eksplisiittinen todistus että viallinen `design`-toimite kaataa
/// `homepage_design`-sopimuksen `fulfill`-todennuksessa.
///
/// Ehdottaa kyvyn täydellä `BrandBrief`-syötteellä, hyväksyy sopimuksen, ajaa
/// `failing()`-suorittimen tuottaman saman viallisen toimitteen `fulfill`:n
/// läpi ja varmistaa: (1) virhe on tulosskeeman rikkomus (puuttuva `headline`),
/// (2) sopimus päätyy [`ContractStatus::Failed`]-tilaan, (3) sama viallinen
/// hyötykuorma ei läpäise tulosskeemaa erilliselläkään `is_valid`-tarkistuksella.
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
    // Tulosskeeman rikkomus (puuttuva headline) on ensimmäinen joka osuu.
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

    // Lisätodiste: sama viallinen hyötykuorma ei ole tulosskeeman mukainen.
    let bad_deliverable = Deliverable::new(agent_alpha, bad_payload, now);
    assert!(
        !cap.output.is_valid(&bad_deliverable.payload),
        "malformed payload is not schema-valid (headline missing)"
    );
}

//! Suoritussauma: orkesteroinnin ja konkreettisen agentin välinen rajapinta.
//!
//! Tämä moduuli määrittelee **yhden jaetun sauman** ([`TurnExecutor`]) jonka
//! kautta [`crate::orchestrator::Orchestrator`] delegoi yksittäisen työvaiheen
//! ("vuoron") konkreettiselle suorittajalle saamatta koskaan riippuvuutta
//! mihinkään tiettyyn agenttiin, LLM-providajaan tai kuljetuskerrokseen.
//!
//! ## Saumamalli
//! Orkesteri tuottaa [`OrchestratedTurn`]-kuvauksen (mitä, kuka, millä
//! syötteellä, milloin) ja antaa sen `TurnExecutor`-toteutukselle. Toteutus
//! palauttaa [`crate::contract::Deliverable`]-toimitteen, joka voidaan ajaa
//! [`crate::contract::ContractBoard::fulfill`]-todennuksen läpi tulosskeemaa ja
//! jälkiehtoja vasten. Näin sopimustakuut säilyvät riippumatta siitä, kuka
//! vuoron oikeasti suoritti.
//!
//! ## Vastuunjako
//! - **Kuluttajapuoli (tämä moduuli):** sauman tyyppi + hermeettinen
//!   [`MockTurnExecutor`] determinististä testausta ja paikallisajoa varten.
//! - **Tuottajapuoli (myöhemmin):** Layer B -tuottaja toteuttaa `LiveTurnExecutor`:n
//!   crateen `familyclaw-agent` **saman** [`TurnExecutor`]-trapin taakse, jolloin
//!   se kytkee oikean LLM-/kuljetuskerroksen muuttamatta orkesteria lainkaan.
//!
//! ## Determinismi
//! [`MockTurnExecutor`] ei lue kelloa eikä käytä satunnaisuutta: toimitteen
//! hyötykuorma riippuu **ainoastaan** [`OrchestratedTurn::input`]-syötteestä ja
//! suorittajan tunnisteesta ([`OrchestratedTurn::assignee`]). Sama vuoro
//! tuottaa siten aina identtisen toimitteen.

use async_trait::async_trait;
use serde_json::{json, Value};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::Timestamp;
use familyclaw_core::Result;

use crate::contract::Deliverable;
use crate::orchestrator::NodeId;
use crate::task::TaskId;

/// Yhden delegoitavan työvaiheen ("vuoron") täysi kuvaus suorittajalle.
///
/// Orkesteri rakentaa tämän kun se osoittaa työnkulun solmun työntekijälle.
/// Kuvaus on itsenäinen: se sisältää suunnitelma- ja solmukontekstin, valitun
/// suorittajan, ihmisluettavan otsikon ja kuvauksen, koneluettavan syötteen
/// sekä injektoidun hetken (`now`). Aikaleima injektoidaan aina — kelloa ei
/// lueta saumassa.
#[derive(Debug, Clone)]
pub struct OrchestratedTurn {
    /// Vuoron synnyttäneen suunnitelman ihmisluettava tunniste.
    pub plan_id: String,

    /// Työnkulun solmun vakaa tunniste (suunnittelijan antama nimi).
    pub node_id: NodeId,

    /// Tehtävätaululle luodun tehtävän tunniste.
    pub task_id: TaskId,

    /// Vuoron suorittava agentti (orkesterin valitsema työntekijä).
    pub assignee: AgentId,

    /// Lyhyt otsikko (peräisin solmun otsikosta).
    pub title: String,

    /// Vapaamuotoinen kuvaus työvaiheesta (peräisin solmusta).
    pub description: String,

    /// Koneluettava syöte vuorolle (validoidaan tyypillisesti kyvyn
    /// syöteskeemaa vasten ennen suoritusta).
    pub input: Value,

    /// Injektoitu suoritushetki (UTC). Saumassa ei koskaan lueta
    /// järjestelmäkelloa — determinismin vuoksi `now` annetaan aina.
    pub now: Timestamp,
}

impl OrchestratedTurn {
    /// Rakentaa vuoron kuvauksen kaikilla kentillä.
    ///
    /// `now` on injektoitava aikaleima (UTC); kutsuja vastaa sen
    /// determinismistä, jotta sauma pysyy toistettavana.
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

/// Saumarajapinta jonka kautta orkesteri suorittaa yhden vuoron.
///
/// Tämä on **se sauma**: [`crate::orchestrator::Orchestrator`] riippuu tästä
/// trapista, ei koskaan konkreettisesta agentista. Kuluttajapuoli (orkesteri +
/// [`MockTurnExecutor`]) elää tässä cratessa; tuottajapuolen (`LiveTurnExecutor`
/// cratessa `familyclaw-agent`) on tarkoitus toteuttaa
/// **sama** rajapinta, jolloin oikea LLM-/kuljetuskerros kytkeytyy muuttamatta
/// orkesteria.
///
/// Toteutusten on oltava [`Send`] + [`Sync`], jotta ne voidaan jakaa
/// tehtävien välillä (`Arc<dyn TurnExecutor>`).
#[async_trait]
pub trait TurnExecutor: Send + Sync {
    /// Suorittaa annetun vuoron ja palauttaa toimitteen.
    ///
    /// Palautettu [`Deliverable`] voidaan ajaa
    /// [`crate::contract::ContractBoard::fulfill`]-todennuksen läpi: vain
    /// tulosskeeman ja jälkiehdot läpäisevä toimite täyttää sopimuksen.
    ///
    /// # Errors
    /// Palauttaa [`familyclaw_core::FamilyClawError`]:n jos suoritus
    /// epäonnistuu (esim. tuottajapuolen kuljetus- tai kykyvirhe). Hermeettinen
    /// [`MockTurnExecutor`] palauttaa virheen vain `failing`-tilan
    /// [`MockFailure::Error`]-variantissa.
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable>;
}

/// Tapa jolla [`MockTurnExecutor`] simuloi epäonnistuvaa suoritusta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockFailure {
    /// Suoritus tuottaa toimitteen, jonka hyötykuorma **rikkoo** tyypillisen
    /// tulosskeeman (puuttuva `headline`-kenttä). Tämä todistaa
    /// `fulfill`-todennuksen `Failed`-polun ilman virhettä suorituksesta.
    SchemaBreach,

    /// Suoritus palauttaa [`Err`]:n simuloiden kuljetus-/kykyvirhettä, jolloin
    /// vuoroa ei koskaan valmistu.
    Error,
}

/// Hermeettinen, deterministinen [`TurnExecutor`]-toteutus testaukseen ja
/// paikallisajoon.
///
/// **Ei verkkoa, ei kelloa, ei satunnaisuutta.** Toimitteen hyötykuorma
/// johdetaan ainoastaan [`OrchestratedTurn::input`]-syötteestä ja
/// [`OrchestratedTurn::assignee`]-suorittajasta, joten sama vuoro tuottaa aina
/// identtisen toimitteen.
///
/// ## Hyötykuorman muoto
/// - Jos syöte on objekti, jossa on kenttä `brand` **tai** `audience`, mock
///   tuottaa `HomepageDesign`-muotoisen toimitteen: `{ headline, sections, cta }`,
///   jossa arvot johdetaan syötteestä deterministisesti.
/// - Muutoin syöte kaiutetaan takaisin avaimen `result` alla yhdessä
///   suorittajan tunnisteen kanssa (`assignee`).
///
/// ## Epäonnistumistila
/// [`MockTurnExecutor::failing`] palauttaa toteutuksen joka tuottaa
/// rikkovan toimitteen tai virheen ([`MockFailure`]) — näin testit voivat
/// todentaa `Failed`-polun.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockTurnExecutor {
    /// `None` = onnistuva (deterministinen) tila; `Some(_)` = simuloitu
    /// epäonnistuminen valitulla tavalla.
    failure: Option<MockFailure>,
}

impl MockTurnExecutor {
    /// Rakentaa onnistuvan, deterministisen mockin.
    #[must_use]
    pub fn new() -> Self {
        Self { failure: None }
    }

    /// Rakentaa mockin joka **rikkoo tulosskeeman** (puuttuva `headline`).
    ///
    /// Tämä on oikotie [`MockTurnExecutor::with_failure`]:lle
    /// [`MockFailure::SchemaBreach`]-tavalla; tuotettu toimite saa
    /// [`crate::contract::ContractBoard::fulfill`]-todennuksen siirtämään
    /// sopimuksen [`crate::contract::ContractStatus::Failed`]-tilaan.
    #[must_use]
    pub fn failing() -> Self {
        Self {
            failure: Some(MockFailure::SchemaBreach),
        }
    }

    /// Rakentaa mockin valitulla epäonnistumistavalla.
    #[must_use]
    pub fn with_failure(failure: MockFailure) -> Self {
        Self {
            failure: Some(failure),
        }
    }

    /// Johtaa onnistuvan toimitteen hyötykuorman puhtaasti vuorosta.
    ///
    /// Riippuu vain `turn.input`:sta ja `turn.assignee`:sta — ei kellosta eikä
    /// satunnaisuudesta.
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
    /// Suorittaa vuoron deterministisesti (tai simuloi epäonnistumisen).
    ///
    /// # Errors
    /// Palauttaa [`familyclaw_core::FamilyClawError::Llm`]:n (simuloitu
    /// tuottajapuolen kuljetus-/kykyvirhe) vain kun mock on rakennettu
    /// [`MockFailure::Error`]-tavalla.
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable> {
        match self.failure {
            Some(MockFailure::Error) => Err(familyclaw_core::FamilyClawError::llm(format!(
                "mock executor failed turn for node {}",
                turn.node_id
            ))),
            Some(MockFailure::SchemaBreach) => {
                // Tarkoituksella ilman `headline`-kenttää → rikkoo tyypillisen
                // HomepageDesign-tulosskeeman, mutta on silti validi toimite.
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

    /// HomepageDesign-muotoinen tulosskeema johon mockin onnistuva toimite
    /// sopii mutta `failing()`-toimite ei.
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
        // Sama syöte + sama suorittaja → identtinen hyötykuorma.
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
        // Ei-homepage-syöte kaiuttaa myös suorittajan tunnisteen → eri payload.
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
        // Sections ei ole tyhjä.
        assert!(!payload["sections"].as_array().expect("array").is_empty());
    }

    #[tokio::test]
    async fn execute_homepage_payload_satisfies_output_schema() {
        // Onnistuvan toimitteen on läpäistävä HomepageDesign-tulosskeema
        // ContractBoard.fulfill-todennuksessa.
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
        // failing() tuottaa toimitteen ilman `headline`-kenttää →
        // HomepageDesign-tulosskeema rikkoutuu Schema.check-tasolla.
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
        // Sama todistus ContractBoard.fulfill-reitin kautta:
        // rikkova toimite siirtää sopimuksen Failed-tilaan.
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
        // Sauma on käytettävissä trait-objektina (orkesteri pitää Arc<dyn _>).
        let exec: std::sync::Arc<dyn TurnExecutor> = std::sync::Arc::new(MockTurnExecutor::new());
        let assignee = AgentId::new();
        let deliverable = exec
            .execute(turn_with(json!({ "x": 1 }), assignee))
            .await
            .expect("execute via dyn");
        assert_eq!(deliverable.from, assignee);
    }
}

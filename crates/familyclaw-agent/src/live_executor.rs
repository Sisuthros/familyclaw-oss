//! Tuottajapuolen suoritussauma: oikealla LLM-kerroksella varustettu
//! [`TurnExecutor`]-toteutus.
//!
//! Tämä moduuli täyttää suunnitellun **tuottajapuolen** aukon: kuluttajapuoli
//! (orkesteri + hermeettinen [`MockTurnExecutor`](familyclaw_bridge::executor::MockTurnExecutor))
//! elää cratessa `familyclaw-bridge`, ja tässä cratessa toteutetaan **sama**
//! [`TurnExecutor`]-rajapinta nimellä [`LiveTurnExecutor`]. Näin orkesteri saa
//! ajaa oikean LLM-pohjaisen vuoron **muuttamatta itseään lainkaan** — se näkee
//! vain trait-objektin `Arc<dyn TurnExecutor>`, kuten ennenkin.
//!
//! ## Riippuvuussuunta
//! `familyclaw-bridge` riippuu vain `familyclaw-core`:sta eikä tästä cratesta.
//! Siksi `familyclaw-agent` saa riippua `familyclaw-bridge`:stä **ilman
//! sykliä**: tuottaja (tämä crate) viittaa saumaan (bridge), ei toisinpäin.
//!
//! ## Determinismi ja kello
//! Toteutus **ei koskaan lue järjestelmäkelloa**: toimitteen aikaleima otetaan
//! aina [`OrchestratedTurn::now`]-kentästä, joka injektoidaan orkesterista.
//! Ainoa ei-deterministinen osa on LLM-kutsu itse; kaikki sen ympärillä oleva
//! logiikka (kehotteen rakennus, vastauksen kääriminen toimitteeksi) on puhdasta
//! ja yksikkötestattu ilman verkkoa.
//!
//! ## Hyötykuorman muoto
//! LLM:n tekstivastaus yritetään jäsentää JSON-**objektiksi**. Jos se on validi
//! JSON-objekti, sitä käytetään sellaisenaan toimitteen hyötykuormana (jolloin
//! sopimuksen tulosskeema voidaan todentaa kentittäin). Muussa tapauksessa
//! (ei-JSON tai JSON joka ei ole objekti, esim. pelkkä merkkijono tai taulukko)
//! teksti kääritään muotoon `{ "result": "<teksti>" }`, jotta hyötykuorma on
//! aina JSON-objekti.

use async_trait::async_trait;
use serde_json::{json, Value};

use familyclaw_bridge::contract::Deliverable;
use familyclaw_bridge::executor::{OrchestratedTurn, TurnExecutor};
use familyclaw_core::{FamilyClawError, ModelConfig, Result};

use crate::llm::LlmMessage;
use crate::llm_chain::{build_llm_chain, LlmEndpointResolver, LlmFailover};

/// Tuottajapuolen [`TurnExecutor`]: ajaa yhden vuoron oikealla LLM-ketjulla.
///
/// Omistaa [`LlmFailover`]-ketjun (primary + fallbackit) ja delegoi varsinaisen
/// täydennyksen sille. Itse sauma-logiikka — kehotteen rakennus syötteestä ja
/// vastauksen kääriminen [`Deliverable`]:ksi — on deterministä ja kellotonta.
pub struct LiveTurnExecutor {
    /// Ajettava failover-ketju, joka suorittaa varsinaisen LLM-täydennyksen.
    chain: LlmFailover,
}

impl LiveTurnExecutor {
    /// Rakentaa suorittajan valmiista [`LlmFailover`]-ketjusta.
    #[must_use]
    pub fn new(chain: LlmFailover) -> Self {
        Self { chain }
    }

    /// Rakentaa suorittajan mallikonfiguraatiosta ([`ModelConfig`]) ja
    /// resolverista: kokoaa failover-ketjun [`build_llm_chain`]illa.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] jos mallikonfiguraatio on kelvoton tai jos
    /// yksikään malli ei ratkennut endpointiksi (ks. [`build_llm_chain`]).
    pub fn from_model(model: &ModelConfig, resolver: &dyn LlmEndpointResolver) -> Result<Self> {
        let chain = build_llm_chain(model, resolver)?;
        Ok(Self::new(chain))
    }

    /// Primary-mallin nimi (raportointiin/lokitukseen).
    #[must_use]
    pub fn primary_model(&self) -> &str {
        self.chain.primary_model()
    }
}

/// Rakentaa LLM-kehotteen vuorosta puhtaasti (ei kelloa, ei satunnaisuutta).
///
/// Yhdistää otsikon, kuvauksen ja koneluettavan syötteen yhdeksi
/// käyttäjäviestiksi. Syöte serialisoidaan vakaasti (pretty JSON), jotta sama
/// vuoro tuottaa aina saman kehotteen. Palauttaa järjestetyn viestilistan
/// (system + user) suoraan [`LlmFailover::complete`]:lle.
fn build_messages(turn: &OrchestratedTurn) -> Vec<LlmMessage> {
    let system = "You are a task executor. Complete the assigned task using the provided \
         input. If the task expects a structured result, respond with a single valid JSON \
         object and nothing else.";

    // Vakaa serialisointi: pretty-printattu JSON on luettava ja deterministinen.
    let input_json =
        serde_json::to_string_pretty(&turn.input).unwrap_or_else(|_| turn.input.to_string());

    let user = format!(
        "# Task: {title}\n\n{description}\n\n## Input\n```json\n{input_json}\n```",
        title = turn.title,
        description = turn.description,
    );

    vec![LlmMessage::system(system), LlmMessage::user(user)]
}

/// Kääräisee LLM:n tekstivastauksen JSON-**objektiksi** toimitteen
/// hyötykuormaksi.
///
/// - Jos `text` jäsentyy validiksi JSON-**objektiksi**, se palautetaan
///   sellaisenaan (sopimuksen tulosskeema voidaan todentaa kentittäin).
/// - Muutoin (ei-JSON, tai JSON joka ei ole objekti) teksti kääritään muotoon
///   `{ "result": "<text>" }`. Näin hyötykuorma on **aina** JSON-objekti.
fn wrap_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Value::Object(map),
        // Validi JSON mutta ei objekti (esim. merkkijono/numero/taulukko) tai
        // täysin jäsentymätön teksti → kääri raakatekstin alle.
        _ => json!({ "result": text }),
    }
}

#[async_trait]
impl TurnExecutor for LiveTurnExecutor {
    /// Suorittaa vuoron oikealla LLM-ketjulla ja kääräisee tuloksen
    /// toimitteeksi.
    ///
    /// Toimitteen `from` on aina [`OrchestratedTurn::assignee`] ja `at` aina
    /// [`OrchestratedTurn::now`] — kelloa ei lueta.
    ///
    /// # Errors
    /// [`FamilyClawError::Llm`] jos koko failover-ketju epäonnistuu (kaikki
    /// klientit antoivat virheen). LLM-virhe kuvataan ydinvirheeksi tekstinä.
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable> {
        let messages = build_messages(&turn);
        let text = self
            .chain
            .complete(&messages)
            .await
            .map_err(|e| FamilyClawError::llm(e.to_string()))?;
        let payload = wrap_payload(&text);
        Ok(Deliverable::new(turn.assignee, payload, turn.now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bridge::executor::OrchestratedTurn;
    use familyclaw_bridge::task::TaskId;
    use familyclaw_core::ids::AgentId;
    use familyclaw_core::time;
    use familyclaw_core::ModelConfig;

    use crate::llm::LlmRole;
    use crate::llm_chain::EnvEndpointResolver;

    fn ts(secs: i64) -> familyclaw_core::time::Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn turn_with(input: Value) -> OrchestratedTurn {
        OrchestratedTurn::new(
            "plan",
            "node",
            TaskId::new(),
            AgentId::new(),
            "Write a greeting",
            "Greet the audience warmly.",
            input,
            ts(1000),
        )
    }

    fn test_resolver() -> EnvEndpointResolver {
        EnvEndpointResolver::new().with_provider(
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY",
        )
    }

    // --- wrap_payload: puhdas JSON-kääräyslogiikka -----------------------

    #[test]
    fn wrap_payload_keeps_valid_json_object() {
        let payload = wrap_payload(r#"{"headline":"Hi","ok":true}"#);
        assert_eq!(payload["headline"], json!("Hi"));
        assert_eq!(payload["ok"], json!(true));
        // Ei "result"-käärettä validin objektin kohdalla.
        assert!(payload.get("result").is_none());
    }

    #[test]
    fn wrap_payload_wraps_plain_text_under_result() {
        let payload = wrap_payload("just some prose, not json");
        assert_eq!(payload["result"], json!("just some prose, not json"));
    }

    #[test]
    fn wrap_payload_wraps_json_array_under_result() {
        // Validi JSON mutta ei objekti → kääritään raakatekstinä result-kenttään.
        let raw = "[1, 2, 3]";
        let payload = wrap_payload(raw);
        assert_eq!(payload["result"], json!(raw));
        assert!(payload.get(0).is_none());
    }

    #[test]
    fn wrap_payload_wraps_bare_json_string_under_result() {
        // `"hello"` on validi JSON (merkkijono) mutta ei objekti.
        let raw = "\"hello\"";
        let payload = wrap_payload(raw);
        assert_eq!(payload["result"], json!(raw));
    }

    #[test]
    fn wrap_payload_always_yields_object() {
        for raw in ["plain", "[1]", "\"s\"", "42", "{\"a\":1}", "not json {"] {
            assert!(
                wrap_payload(raw).is_object(),
                "payload for {raw:?} must be a JSON object"
            );
        }
    }

    // --- build_messages: puhdas kehotteen rakennus -----------------------

    #[test]
    fn build_messages_has_system_then_user() {
        let msgs = build_messages(&turn_with(json!({ "x": 1 })));
        assert_eq!(msgs.len(), 2);
        // Ensimmäinen on system, toinen user (järjestys taattu).
        assert_eq!(msgs[0].role, LlmRole::System);
        assert_eq!(msgs[1].role, LlmRole::User);
    }

    #[test]
    fn build_messages_user_contains_title_description_and_input() {
        let msgs = build_messages(&turn_with(json!({ "brand": "DuckUps" })));
        let user = &msgs[1].content;
        assert!(user.contains("Write a greeting"), "title present");
        assert!(user.contains("Greet the audience warmly."), "desc present");
        assert!(user.contains("DuckUps"), "input value present");
    }

    #[test]
    fn build_messages_is_deterministic_for_same_turn() {
        let a = build_messages(&turn_with(json!({ "topic": "rust" })));
        let b = build_messages(&turn_with(json!({ "topic": "rust" })));
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0].content, b[0].content);
        assert_eq!(a[1].content, b[1].content);
    }

    // --- konstruktorit (ei verkkoa) --------------------------------------

    #[test]
    fn from_model_builds_executor_without_network() {
        // Avainta ei tarvita rakennukseen; virhe näkyisi vasta complete()-kutsussa.
        let resolver = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o");
        let exec = LiveTurnExecutor::from_model(&model, &resolver).expect("builds");
        assert_eq!(exec.primary_model(), "openai/gpt-4o");
    }

    #[test]
    fn from_model_errors_when_nothing_resolves() {
        let resolver = test_resolver();
        let model = ModelConfig::new("mystery/model");
        assert!(LiveTurnExecutor::from_model(&model, &resolver).is_err());
    }

    #[test]
    fn live_executor_is_usable_as_dyn_trait_object() {
        // Sauma: orkesteri pitää Arc<dyn TurnExecutor>. Todistetaan että
        // LiveTurnExecutor sopii siihen (vain tyyppitason todiste, ei ajoa).
        let resolver = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o");
        let exec = LiveTurnExecutor::from_model(&model, &resolver).expect("builds");
        let _erased: std::sync::Arc<dyn TurnExecutor> = std::sync::Arc::new(exec);
    }
}

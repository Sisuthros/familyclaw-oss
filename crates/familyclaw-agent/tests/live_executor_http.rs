//! Integraatiotesti: todistaa että [`LiveTurnExecutor::execute`] ajaa oikean
//! HTTP-LLM-polun päästä päähän mock-palvelinta vasten.
//!
//! Tämä sulkee README:n myöntämän luottamusaukon "built but unproven": ennen
//! tätä testiä `live_executor`-yksikkötestit kattoivat vain *puhtaat* osat
//! (kehotteen rakennus, hyötykuorman kääräys, konstruktorit) — varsinainen
//! `execute()` ei ajanut edes mockia vasten, joten oikea reqwest → vastaus →
//! [`Deliverable`] -polku oli todistamaton.
//!
//! Mock on pelkkä `std::net::TcpListener` (ei `wiremock`/`httpmock`-dependencyä),
//! joten tämä ei lisää yhtään dev-dependencyä eikä riko `cargo-deny`-gatea. Se
//! palauttaa OpenAI-yhteensopivan `chat.completion`-rungon, jolloin koko
//! `LlmFailover::complete`-ketju ajetaan oikealla HTTP-kuljetuksella.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use familyclaw_agent::live_executor::LiveTurnExecutor;
use familyclaw_agent::llm_chain::{build_llm_chain, EnvEndpointResolver};
use familyclaw_bridge::executor::{OrchestratedTurn, TurnExecutor};
use familyclaw_bridge::TaskId;
use familyclaw_core::ids::AgentId;
use familyclaw_core::{time, ModelConfig};

/// Yksi skriptattu HTTP-vastaus: status + assistant-sisältö (200-tapauksessa).
#[derive(Clone)]
struct Reply {
    status: u16,
    content: String,
}

impl Reply {
    fn ok(content: &str) -> Self {
        Self {
            status: 200,
            content: content.into(),
        }
    }
    fn status(code: u16) -> Self {
        Self {
            status: code,
            content: String::new(),
        }
    }
}

/// Minimaalinen HTTP/1.1-mock ilman axumia: lukee pyynnön, valitsee
/// `script[min(call, len-1)]`-vastauksen (saturoi viimeiseen) ja vastaa
/// OpenAI-yhteensopivalla rungolla. Laskee pyynnöt.
struct MockLlm {
    base_url: String,
    calls: Arc<AtomicUsize>,
}

impl MockLlm {
    fn spawn(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
        let addr = listener.local_addr().expect("mock local_addr");
        let base_url = format!("http://{addr}/v1");
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_t = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let n = calls_t.fetch_add(1, Ordering::SeqCst);
                Self::handle(stream, n, &script);
            }
        });

        Self { base_url, calls }
    }

    fn total_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn handle(mut stream: TcpStream, call_index: usize, script: &[Reply]) {
        // Lue request (headerit + alku bodysta) yhteen bufferiin; emme tarvitse
        // koko bodya, vain triggerin vastaukselle.
        let mut buf = [0_u8; 4096];
        let _ = stream.read(&mut buf).unwrap_or(0);

        let idx = call_index.min(script.len().saturating_sub(1));
        let reply = script
            .get(idx)
            .cloned()
            .unwrap_or_else(|| Reply::status(500));

        let body = if reply.status == 200 {
            format!(
                r#"{{"id":"x","object":"chat.completion","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}]}}"#,
                serde_json::to_string(&reply.content).expect("json string")
            )
        } else {
            r#"{"error":"mock"}"#.to_string()
        };
        let reason = match reply.status {
            200 => "OK",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.status,
            reason,
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

fn ts(secs: i64) -> time::Timestamp {
    time::from_unix_secs(secs).expect("valid unix seconds")
}

/// Rakentaa vuoron jolla on tunnettu assignee + now, jotta toimitteen kentät
/// voidaan tarkistaa.
fn turn(assignee: AgentId, input: Value) -> OrchestratedTurn {
    OrchestratedTurn::new(
        "plan",
        "node",
        TaskId::new(),
        assignee,
        "Write a greeting",
        "Greet the audience warmly.",
        input,
        ts(1000),
    )
}

/// Rakentaa [`LiveTurnExecutor`]:n yhden mallin ketjusta joka osoittaa mockiin.
fn live_executor_pointing_at(mock: &MockLlm) -> LiveTurnExecutor {
    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        mock.base_url.clone(),
        "FAMILYCLAW_TEST_KEY_UNSET",
    );
    let model = ModelConfig::new("mock/model-a");
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    LiveTurnExecutor::new(chain)
}

/// Rakentaa kahden mallin (primary + fallback) ketjun mockiin: failover-todiste.
fn live_executor_with_fallback(mock: &MockLlm) -> LiveTurnExecutor {
    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        mock.base_url.clone(),
        "FAMILYCLAW_TEST_KEY_UNSET",
    );
    // Sama provider, kaksi mallia: ketju yrittää primaryn, sitten fallbackin.
    // Molemmat osuvat samaan mockiin, joka skriptaa vastaukset kutsujärjestyksellä.
    let mut model = ModelConfig::new("mock/primary");
    model.fallbacks.push("mock/fallback".to_string());
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    LiveTurnExecutor::new(chain)
}

#[tokio::test]
async fn execute_returns_json_object_payload_over_real_http() {
    // Mock palauttaa validin JSON-objektin → toimitteen hyötykuorma on se objekti
    // sellaisenaan (ei "result"-kääräystä), todistettuna oikean HTTP-rungon yli.
    let mock = MockLlm::spawn(vec![Reply::ok(r#"{"headline":"Hi","ok":true}"#)]);
    let assignee = AgentId::new();
    let exec = live_executor_pointing_at(&mock);

    let deliverable = exec
        .execute(turn(assignee, json!({ "brand": "DuckUps" })))
        .await
        .expect("execute succeeds against mock");

    assert_eq!(deliverable.payload["headline"], json!("Hi"));
    assert_eq!(deliverable.payload["ok"], json!(true));
    assert!(
        deliverable.payload.get("result").is_none(),
        "valid JSON object must not be wrapped under result"
    );
    // Sauma-invariantit: from = assignee, at = injektoitu now (ei kelloa).
    assert_eq!(deliverable.from, assignee);
    assert_eq!(deliverable.at, ts(1000));
    assert_eq!(mock.total_calls(), 1, "exactly one LLM call for one turn");
}

#[tokio::test]
async fn execute_wraps_plain_text_under_result_over_real_http() {
    // Mock palauttaa ei-JSON-tekstiä → hyötykuorma kääritään { result: text }.
    // Todistaa wrap_payload-integraation oikean HTTP-vastauksen yli.
    let mock = MockLlm::spawn(vec![Reply::ok("just some prose, not json")]);
    let exec = live_executor_pointing_at(&mock);

    let deliverable = exec
        .execute(turn(AgentId::new(), json!({ "x": 1 })))
        .await
        .expect("execute succeeds");

    assert_eq!(
        deliverable.payload["result"],
        json!("just some prose, not json")
    );
}

#[tokio::test]
async fn execute_surfaces_provider_error_as_err() {
    // Yhden mallin ketju + mock palauttaa 503 → koko ketju epäonnistuu →
    // execute() palauttaa Err (ei tyhjää/roskaista Deliverablea). Todistaa että
    // providerivirhe propagoituu virheeksi, ei valehtelevaksi toimitteeksi.
    let mock = MockLlm::spawn(vec![Reply::status(503)]);
    let exec = live_executor_pointing_at(&mock);

    let result = exec.execute(turn(AgentId::new(), json!({}))).await;

    assert!(
        result.is_err(),
        "a dead single-model chain must surface an error, not a Deliverable"
    );
}

#[tokio::test]
async fn execute_recovers_via_fallback_over_real_http() {
    // Primary 503, fallback 200 → execute() palauttaa fallbackin sisällön.
    // Todistaa että LlmFailover.complete-failover toimii execute():n kautta
    // päästä päähän oikealla HTTP-kuljetuksella.
    let mock = MockLlm::spawn(vec![Reply::status(503), Reply::ok("recovered by fallback")]);
    let exec = live_executor_with_fallback(&mock);

    let deliverable = exec
        .execute(turn(AgentId::new(), json!({})))
        .await
        .expect("fallback recovers the turn");

    assert_eq!(
        deliverable.payload["result"],
        json!("recovered by fallback"),
        "fallback model's content should reach the deliverable"
    );
    assert_eq!(
        mock.total_calls(),
        2,
        "primary failed once, fallback succeeded once"
    );
}

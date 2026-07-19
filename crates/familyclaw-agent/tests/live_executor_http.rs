//! Integration test: proves that [`LiveTurnExecutor::execute`] runs the
//! real HTTP LLM path end to end against a mock server.
//!
//! This closes the trust gap the README itself admitted to ("built but
//! unproven"): before this test, `live_executor`'s unit tests only
//! covered the *pure* parts (prompt construction, payload wrapping,
//! constructors) — the actual `execute()` never ran against even a mock,
//! so the real reqwest → response → [`Deliverable`] path was unproven.
//!
//! The mock is a plain `std::net::TcpListener` (no `wiremock`/`httpmock`
//! dependency), so this adds no dev-dependency and doesn't break the
//! `cargo-deny` gate. It returns an OpenAI-compatible
//! `chat.completion` body, so the entire `LlmFailover::complete` chain
//! runs over a real HTTP transport.

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

/// A single scripted HTTP response: status + assistant content (for the
/// 200 case).
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

/// A minimal HTTP/1.1 mock without axum: reads the request, picks the
/// `script[min(call, len-1)]` response (saturating at the last one), and
/// replies with an OpenAI-compatible body. Counts requests.
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
        // Read the request (headers + start of the body) into one buffer;
        // we don't need the whole body, just the trigger for the response.
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

/// Builds a turn with a known assignee + now, so the deliverable's fields
/// can be checked.
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

/// Builds a [`LiveTurnExecutor`] from a single-model chain pointing at
/// the mock.
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

/// Builds a two-model (primary + fallback) chain against the mock: proof
/// of failover.
fn live_executor_with_fallback(mock: &MockLlm) -> LiveTurnExecutor {
    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        mock.base_url.clone(),
        "FAMILYCLAW_TEST_KEY_UNSET",
    );
    // Same provider, two models: the chain tries the primary, then the
    // fallback. Both hit the same mock, which scripts responses by call order.
    let mut model = ModelConfig::new("mock/primary");
    model.fallbacks.push("mock/fallback".to_string());
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    LiveTurnExecutor::new(chain)
}

#[tokio::test]
async fn execute_returns_json_object_payload_over_real_http() {
    // The mock returns a valid JSON object → the deliverable's payload is
    // that object as-is (no "result" wrapping), proven over a real HTTP body.
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
    // Joint invariants: from = assignee, at = injected now (no clock read).
    assert_eq!(deliverable.from, assignee);
    assert_eq!(deliverable.at, ts(1000));
    assert_eq!(mock.total_calls(), 1, "exactly one LLM call for one turn");
}

#[tokio::test]
async fn execute_wraps_plain_text_under_result_over_real_http() {
    // The mock returns non-JSON text → the payload is wrapped as
    // { result: text }. Proves the wrap_payload integration over a real
    // HTTP response.
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
    // A single-model chain + the mock returns 503 → the whole chain fails →
    // execute() returns Err (not an empty/garbage Deliverable). Proves
    // that a provider error propagates as an error, not a lying deliverable.
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
    // Primary 503, fallback 200 → execute() returns the fallback's content.
    // Proves that LlmFailover.complete's failover works through execute()
    // end to end over a real HTTP transport.
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

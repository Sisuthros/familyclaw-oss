//! Producer-side execution joint: a [`TurnExecutor`] implementation backed
//! by a real LLM layer.
//!
//! This module fills the planned **producer-side** gap: the consumer side
//! (the orchestrator + the hermetic
//! [`MockTurnExecutor`](familyclaw_bridge::executor::MockTurnExecutor))
//! lives in the `familyclaw-bridge` crate, and this crate implements the
//! **same** [`TurnExecutor`] interface under the name [`LiveTurnExecutor`].
//! This lets the orchestrator run a real LLM-backed turn **without
//! changing itself at all** — it only ever sees the trait object
//! `Arc<dyn TurnExecutor>`, as before.
//!
//! ## Dependency direction
//! `familyclaw-bridge` depends only on `familyclaw-core`, not on this
//! crate. That's why `familyclaw-agent` is allowed to depend on
//! `familyclaw-bridge` **without a cycle**: the producer (this crate)
//! refers to the joint (bridge), not the other way around.
//!
//! ## Determinism and the clock
//! The implementation **never reads the system clock**: the deliverable's
//! timestamp is always taken from the [`OrchestratedTurn::now`] field,
//! which is injected by the orchestrator. The only non-deterministic part
//! is the LLM call itself; all the logic around it (prompt construction,
//! wrapping the response into a deliverable) is pure and unit-tested
//! without a network.
//!
//! ## Payload shape
//! The LLM's text response is attempted to be parsed as a JSON
//! **object**. If it is a valid JSON object, it is used as-is as the
//! deliverable's payload (so the contract's result schema can be
//! validated field by field). Otherwise (non-JSON, or JSON that isn't an
//! object, e.g. a bare string or array), the text is wrapped as
//! `{ "result": "<text>" }`, so the payload is always a JSON object.

use async_trait::async_trait;
use serde_json::{json, Value};

use familyclaw_bridge::contract::Deliverable;
use familyclaw_bridge::executor::{OrchestratedTurn, TurnExecutor};
use familyclaw_core::{FamilyClawError, ModelConfig, Result};

use crate::llm::LlmMessage;
use crate::llm_chain::{build_llm_chain, LlmEndpointResolver, LlmFailover};

/// Producer-side [`TurnExecutor`]: runs a single turn using a real LLM
/// chain.
///
/// Owns an [`LlmFailover`] chain (primary + fallbacks) and delegates the
/// actual completion to it. The joint logic itself — building the prompt
/// from the input and wrapping the response into a [`Deliverable`] — is
/// deterministic and clock-free.
pub struct LiveTurnExecutor {
    /// The failover chain to run, which performs the actual LLM
    /// completion.
    chain: LlmFailover,
}

impl LiveTurnExecutor {
    /// Builds the executor from a ready-made [`LlmFailover`] chain.
    #[must_use]
    pub fn new(chain: LlmFailover) -> Self {
        Self { chain }
    }

    /// Builds the executor from a model configuration ([`ModelConfig`])
    /// and a resolver: assembles the failover chain via
    /// [`build_llm_chain`].
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] if the model configuration is invalid
    /// or if no model resolved to an endpoint (see [`build_llm_chain`]).
    pub fn from_model(model: &ModelConfig, resolver: &dyn LlmEndpointResolver) -> Result<Self> {
        let chain = build_llm_chain(model, resolver)?;
        Ok(Self::new(chain))
    }

    /// The primary model's name (for reporting/logging).
    #[must_use]
    pub fn primary_model(&self) -> &str {
        self.chain.primary_model()
    }
}

/// Builds the LLM prompt from a turn, purely (no clock, no randomness).
///
/// Combines the title, description, and machine-readable input into a
/// single user message. The input is serialized stably (pretty JSON) so
/// the same turn always produces the same prompt. Returns the ordered
/// message list (system + user) ready for [`LlmFailover::complete`].
fn build_messages(turn: &OrchestratedTurn) -> Vec<LlmMessage> {
    let system = "You are a task executor. Complete the assigned task using the provided \
         input. If the task expects a structured result, respond with a single valid JSON \
         object and nothing else.";

    // Stable serialization: pretty-printed JSON is readable and deterministic.
    let input_json =
        serde_json::to_string_pretty(&turn.input).unwrap_or_else(|_| turn.input.to_string());

    let user = format!(
        "# Task: {title}\n\n{description}\n\n## Input\n```json\n{input_json}\n```",
        title = turn.title,
        description = turn.description,
    );

    vec![LlmMessage::system(system), LlmMessage::user(user)]
}

/// Wraps the LLM's text response into a JSON **object** as the
/// deliverable's payload.
///
/// - If `text` parses as a valid JSON **object**, it is returned as-is
///   (the contract's result schema can be validated field by field).
/// - Otherwise (non-JSON, or JSON that isn't an object) the text is
///   wrapped as `{ "result": "<text>" }`. This way the payload is
///   **always** a JSON object.
fn wrap_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Value::Object(map),
        // Valid JSON but not an object (e.g. string/number/array), or
        // completely unparseable text → wrap under the raw text.
        _ => json!({ "result": text }),
    }
}

#[async_trait]
impl TurnExecutor for LiveTurnExecutor {
    /// Executes the turn using the real LLM chain and wraps the result
    /// into a deliverable.
    ///
    /// The deliverable's `from` is always [`OrchestratedTurn::assignee`]
    /// and `at` is always [`OrchestratedTurn::now`] — the clock is never
    /// read.
    ///
    /// # Errors
    /// [`FamilyClawError::Llm`] if the whole failover chain fails (all
    /// clients returned an error). The LLM error is described as a core
    /// error, as text.
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

    // --- wrap_payload: pure JSON-wrapping logic ---------------------------

    #[test]
    fn wrap_payload_keeps_valid_json_object() {
        let payload = wrap_payload(r#"{"headline":"Hi","ok":true}"#);
        assert_eq!(payload["headline"], json!("Hi"));
        assert_eq!(payload["ok"], json!(true));
        // No "result" wrapper for a valid object.
        assert!(payload.get("result").is_none());
    }

    #[test]
    fn wrap_payload_wraps_plain_text_under_result() {
        let payload = wrap_payload("just some prose, not json");
        assert_eq!(payload["result"], json!("just some prose, not json"));
    }

    #[test]
    fn wrap_payload_wraps_json_array_under_result() {
        // Valid JSON but not an object → wrapped as raw text under the result field.
        let raw = "[1, 2, 3]";
        let payload = wrap_payload(raw);
        assert_eq!(payload["result"], json!(raw));
        assert!(payload.get(0).is_none());
    }

    #[test]
    fn wrap_payload_wraps_bare_json_string_under_result() {
        // `"hello"` is valid JSON (a string) but not an object.
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

    // --- build_messages: pure prompt construction -------------------------

    #[test]
    fn build_messages_has_system_then_user() {
        let msgs = build_messages(&turn_with(json!({ "x": 1 })));
        assert_eq!(msgs.len(), 2);
        // The first is system, the second is user (order guaranteed).
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

    // --- constructors (no network) ----------------------------------------

    #[test]
    fn from_model_builds_executor_without_network() {
        // No key is needed for construction; an error would only show up
        // on the complete() call.
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
        // Joint: the orchestrator holds Arc<dyn TurnExecutor>. Prove that
        // LiveTurnExecutor fits it (a type-level proof only, no execution).
        let resolver = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o");
        let exec = LiveTurnExecutor::from_model(&model, &resolver).expect("builds");
        let _erased: std::sync::Arc<dyn TurnExecutor> = std::sync::Arc::new(exec);
    }
}

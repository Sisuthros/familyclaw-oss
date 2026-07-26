//! LLM HTTP client — multi-provider wire-format layer.
//!
//! Generic client that calls any provider endpoint reachable through one of
//! the wire formats in [`LlmWireFormat`]. Configuration is loaded at runtime
//! — never hardcoded.
//!
//! **Layer A only:** No family-specific names, souls, or private data.
//!
//! ## Wire formats (ANY-AI provider support)
//!
//! [`LlmConfig::wire_format`] selects the request/response shape used to talk
//! to the configured `api_base`:
//!
//! - [`LlmWireFormat::OpenAiChat`] (default) — `POST {api_base}/chat/completions`,
//!   the original and most complete path: text, streaming, and tool calling.
//! - [`LlmWireFormat::GeminiGenerate`] — native Google Gemini
//!   `POST {api_base}/models/{model}:generateContent?key={api_key}`. **Verified
//!   minimal slice:** plain text completion ([`LlmClient::complete`]) is fully
//!   implemented and tested. Tool calling and streaming are NOT yet
//!   implemented for this wire format — calling
//!   [`LlmClient::complete_with_tools`] with a non-empty tool list, or
//!   [`LlmClient::complete_stream`], returns [`LlmError::Http`] with a message
//!   pointing at `docs/design/multi-provider-wire-formats.md`.
//! - [`LlmWireFormat::AnthropicMessages`] / [`LlmWireFormat::Bedrock`] —
//!   **design-doc only, not implemented.** Selecting either returns
//!   [`LlmError::Http`] immediately, pointing at the same design doc. See
//!   `docs/design/multi-provider-wire-formats.md` for the planned wire shape
//!   and the SigV4-signing plan for Bedrock.

use std::pin::Pin;
use std::time::Duration;

use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Default **request** timeout (the whole request, incl. reading the
/// response) if [`LlmConfig::request_timeout_ms`] is not set. 60 s is a
/// sensible upper bound for an LLM completion: loose enough for a slow model,
/// but tight enough that a stuck primary doesn't block failover forever (F1).
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;

/// Default **connect** timeout (TCP/TLS handshake) if
/// [`LlmConfig::connect_timeout_ms`] is not set. 10 s distinguishes an
/// "endpoint not responding at all" situation from slow generation — the
/// connect phase must not lean on the full 60 s request budget.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;

/// Default max output tokens per completion if [`LlmConfig::max_tokens`] is not
/// otherwise set. Raised from 2048 -> 4096: 2048 was cutting off longer replies
/// mid-sentence (agent replies routinely exceed it). Layer B can still raise
/// this further per deployment via `FAMILYCLAW_MAX_TOKENS`
/// (`familyclaw-gateway/src/main.rs`) or [`LlmConfig::with_max_tokens`].
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Default max number of auto-continuation rounds when a completion is cut
/// off by the token limit (`finish_reason == "length"`). Each round appends
/// the partial reply + a "continue" nudge and calls again, concatenating the
/// result — this is what actually fixes replies getting cut mid-sentence
/// (raising `max_tokens` alone only raises the ceiling, it doesn't guarantee
/// the model stops cleanly under it). `0` disables continuation entirely.
/// Overridable via `FAMILYCLAW_MAX_CONTINUATIONS`.
pub const DEFAULT_MAX_CONTINUATIONS: u32 = 3;

/// Safety cap on the total accumulated reply length (bytes) across all
/// continuation rounds. Bounds worst-case cost/latency even if
/// `FAMILYCLAW_MAX_CONTINUATIONS` is set very high — continuation stops once
/// the accumulated text reaches this size, regardless of remaining rounds.
pub const MAX_CONTINUATION_OUTPUT_CHARS: usize = 64_000;

/// Reads the `FAMILYCLAW_MAX_CONTINUATIONS` environment variable, or returns
/// [`DEFAULT_MAX_CONTINUATIONS`]. Unlike most env-var readers in this crate,
/// `0` is a valid, meaningful value ("continuation disabled") and is NOT
/// treated as "unset" — only a missing/unparseable value falls back to the
/// default (same shape as [`crate::watchdog::turn_watchdog_secs`], minus the
/// `> 0` filter).
#[must_use]
pub fn max_continuations() -> u32 {
    std::env::var("FAMILYCLAW_MAX_CONTINUATIONS")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_MAX_CONTINUATIONS)
}

/// The user-turn nudge appended before each continuation call.
const CONTINUATION_NUDGE: &str = "Continue exactly where you left off. Do not repeat or summarize.";

/// Truncates `s` to at most `max_bytes`, respecting the UTF-8 char boundary
/// (never splits a multi-byte character). Mirrors the boundary-safe pattern
/// used by `familyclaw_agent::agent::truncate_for_history`.
fn safe_truncate_to(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Why the model stopped generating (`OpenAI` chat-completions
/// `choices[].finish_reason`).
///
/// Captured so callers (the auto-continuation loop in [`LlmClient::complete`])
/// can react to a `length` stop instead of silently shipping a truncated
/// reply. An absent or unrecognized value defaults to [`FinishReason::Stop`]
/// (the safe assumption — "the model appears to be done", not "something
/// broke"), so older fixtures / providers that omit the field keep working
/// exactly as before this field existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// The model completed its response normally.
    Stop,
    /// The response was cut off by the `max_tokens` limit — the caller may
    /// want to continue the generation.
    Length,
    /// The model chose to call one or more tools instead of replying with
    /// text.
    ToolCalls,
    /// A provider-specific value this crate doesn't special-case (e.g.
    /// `"content_filter"`). Treated like `Stop` for continuation purposes.
    Other(String),
}

impl FinishReason {
    /// Maps the wire's `finish_reason` string (or its absence) to a
    /// [`FinishReason`]. `None`/unrecognized -> [`FinishReason::Stop`].
    fn from_wire(raw: Option<&str>) -> Self {
        match raw {
            Some("stop") | None => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some("tool_calls") => FinishReason::ToolCalls,
            Some(other) => FinishReason::Other(other.to_string()),
        }
    }

    /// True only for [`FinishReason::Length`] — the sole trigger for
    /// auto-continuation.
    #[must_use]
    pub const fn is_length(&self) -> bool {
        matches!(self, FinishReason::Length)
    }

    /// Maps Gemini's `candidates[].finishReason` string (or its absence) to a
    /// [`FinishReason`]. Gemini's vocabulary differs from `OpenAI`'s
    /// (`"STOP"`/`"MAX_TOKENS"` vs. `"stop"`/`"length"`), so this is a
    /// separate mapping rather than reusing [`Self::from_wire`].
    /// `None`/unrecognized -> [`FinishReason::Stop`] (same safe default as
    /// [`Self::from_wire`]).
    fn from_gemini_wire(raw: Option<&str>) -> Self {
        match raw {
            Some("STOP") | None => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::Length,
            Some(other) => FinishReason::Other(other.to_string()),
        }
    }
}

/// Which wire format [`LlmClient`] uses to talk to [`LlmConfig::api_base`].
///
/// This is the ANY-AI provider layer: the request/response shape a provider
/// speaks is independent of the failover/resolver machinery in
/// [`crate::llm_chain`], so a Gemini or Anthropic entry can sit in the same
/// [`crate::llm_chain::LlmFailover`] chain as an OpenAI-compatible one. See
/// the module docs above and `docs/design/multi-provider-wire-formats.md`
/// for the status of each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmWireFormat {
    /// `POST {api_base}/chat/completions` — `OpenAI`-compatible chat
    /// completions (the original, fully-featured wire format: text,
    /// streaming, tool calling). Default for backward compatibility — every
    /// [`LlmConfig`] built before this enum existed behaves identically.
    #[default]
    OpenAiChat,
    /// `POST {api_base}/models/{model}:generateContent?key={api_key}` —
    /// native Google Gemini `generateContent`. Verified minimal slice: text
    /// completion only (see module docs).
    GeminiGenerate,
    /// Anthropic `POST {api_base}/v1/messages`. **Not implemented** —
    /// selecting this returns [`LlmError::Http`] at call time. Design:
    /// `docs/design/multi-provider-wire-formats.md`.
    AnthropicMessages,
    /// AWS Bedrock `InvokeModel`/`Converse`, SigV4-signed. **Not
    /// implemented** — selecting this returns [`LlmError::Http`] at call
    /// time. Design: `docs/design/multi-provider-wire-formats.md`.
    Bedrock,
}

impl LlmWireFormat {
    /// One-word tag for logs/errors (stable).
    #[must_use]
    pub const fn as_word(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai_chat",
            Self::GeminiGenerate => "gemini_generate",
            Self::AnthropicMessages => "anthropic_messages",
            Self::Bedrock => "bedrock",
        }
    }
}

/// LLM configuration — loaded at runtime from env/file (never hardcoded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// API base URL, e.g. `https://api.openai.com/v1`
    pub api_base: String,
    /// API key (loaded from environment or config file at runtime)
    pub api_key: String,
    /// Model name (e.g., "gpt-4o", "llama3")
    pub model: String,
    /// Maximum tokens in response
    pub max_tokens: u32,
    /// Wire format used to talk to `api_base` (ANY-AI provider support).
    /// `#[serde(default)]` → configs serialized before this field existed
    /// deserialize as [`LlmWireFormat::OpenAiChat`], unchanged behavior.
    #[serde(default)]
    pub wire_format: LlmWireFormat,
    /// Timeout for the whole request (request + reading the response) in
    /// milliseconds. `None` → [`DEFAULT_REQUEST_TIMEOUT_MS`]. Layer B can tune
    /// this per provider (e.g. tighter for a fast endpoint, looser for a slow
    /// one). **F1:** without a timeout, a stuck primary would block
    /// [`crate::llm_chain::LlmFailover::complete`] forever — the timeout
    /// forces a dead primary to give up, which triggers failover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
    /// Timeout for establishing the TCP/TLS connection, in milliseconds.
    /// `None` → [`DEFAULT_CONNECT_TIMEOUT_MS`]. Distinguishes a
    /// "not listening / no route" situation from slow generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u64>,
}

impl LlmConfig {
    /// Creates a new LLM config.
    #[must_use]
    pub fn new(
        api_base: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            wire_format: LlmWireFormat::OpenAiChat,
            request_timeout_ms: None,
            connect_timeout_ms: None,
        }
    }

    /// Sets maximum tokens.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sets the wire format (ANY-AI provider support). Default
    /// [`LlmWireFormat::OpenAiChat`]. See the module docs for which formats
    /// are fully implemented vs. design-doc-only.
    #[must_use]
    pub fn with_wire_format(mut self, wire_format: LlmWireFormat) -> Self {
        self.wire_format = wire_format;
        self
    }

    /// Sets the whole-request timeout (in milliseconds). Layer B tuning per
    /// provider. See [`DEFAULT_REQUEST_TIMEOUT_MS`].
    #[must_use]
    pub fn with_request_timeout_ms(mut self, ms: u64) -> Self {
        self.request_timeout_ms = Some(ms);
        self
    }

    /// Sets the connection-establishment timeout (in milliseconds). Layer B
    /// tuning per provider. See [`DEFAULT_CONNECT_TIMEOUT_MS`].
    #[must_use]
    pub fn with_connect_timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = Some(ms);
        self
    }

    /// The effective request timeout as a [`Duration`] (default filled in).
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(
            self.request_timeout_ms
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        )
    }

    /// The effective connect timeout as a [`Duration`] (default filled in).
    #[must_use]
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(
            self.connect_timeout_ms
                .unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS),
        )
    }
}

/// Role of an LLM message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmRole {
    /// System-level instruction.
    System,
    /// User message.
    User,
    /// Assistant response.
    Assistant,
    /// Tool result.
    Tool,
}

/// A message for the LLM chat completions API.
///
/// `PartialEq` (not `Eq`) because [`ToolCall::arguments`] is a
/// `serde_json::Value`, which is only `PartialEq` (it may contain floats).
/// This lets the resumable-turn state ([`crate::resumable::ResumableTurn`]) and
/// the tool-loop control type compare message stacks in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    /// Role of the message sender
    pub role: LlmRole,
    /// Message content
    pub content: String,
    /// Optional tool call ID (for tool role messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional tool calls (for assistant role)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl LlmMessage {
    /// Creates a system message.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Creates a user message.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Creates an assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Creates a tool response message.
    #[must_use]
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Tool,
            content: content.into(),
            tool_call_id: Some(id.into()),
            tool_calls: None,
        }
    }

    /// Sets tool calls for this message.
    #[must_use]
    pub fn with_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.tool_calls = Some(calls);
        self
    }

    /// Returns true if this message has tool calls.
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
    }
}

/// A tool call request from the LLM.
///
/// `PartialEq` (not `Eq`) because [`arguments`](Self::arguments) is a
/// `serde_json::Value` (only `PartialEq` — may contain floats).
///
/// **Wire shape vs. in-memory shape.** In memory this is a flat
/// `{id, name, arguments}` triple with `arguments` as a parsed
/// [`serde_json::Value`]. On the wire it is the `OpenAI` chat-completions tool
/// call shape:
///
/// ```json
/// {"id": "call_abc", "type": "function",
///  "function": {"name": "fs_read", "arguments": "{\"path\":\"a.txt\"}"}}
/// ```
///
/// Note that `function.arguments` is a **JSON-encoded string**, not a nested
/// object. Custom [`Serialize`]/[`Deserialize`] impls below bridge the two
/// forms, so the derived flat shape never reaches the wire. This is why a tool
/// call response decoded with the *derived* impl failed: serde looked for a
/// top-level `name`/`arguments`, but the provider nests them under `function`
/// and stringifies the arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Unique ID for this tool call
    pub id: String,
    /// Tool name
    pub name: String,
    /// Arguments as JSON
    pub arguments: serde_json::Value,
}

/// `OpenAI` wire shape for a single tool call — the envelope the chat
/// completions API both **emits** (in assistant responses) and **expects**
/// (when an assistant message with tool calls is replayed in a request).
///
/// `function.arguments` is a JSON-encoded **string** on the wire. We keep it as
/// a raw [`String`] here and (de)serialize it to/from [`ToolCall::arguments`]
/// (a parsed [`serde_json::Value`]) in the [`ToolCall`] serde impls.
#[derive(Serialize, Deserialize)]
struct ToolCallWire {
    id: String,
    /// Always `"function"` for the only tool kind we support. `#[serde(default)]`
    /// tolerates providers that omit it on the response.
    #[serde(rename = "type", default = "tool_call_kind_function")]
    kind: String,
    function: ToolCallFunctionWire,
}

#[derive(Serialize, Deserialize)]
struct ToolCallFunctionWire {
    name: String,
    /// JSON-encoded arguments. Standard providers send a string
    /// (e.g. `"{\"path\":\"a.txt\"}"`); see [`ToolCall`]'s `Deserialize` for how
    /// a non-string (some providers send a raw object) is tolerated.
    arguments: ArgumentsWire,
}

/// Tolerates both the standard string-encoded `arguments` and the non-standard
/// raw-object form some `OpenAI`-compatible providers emit. Always serializes
/// back to the standard JSON **string** form on the wire.
#[derive(Deserialize)]
#[serde(untagged)]
enum ArgumentsWire {
    /// Standard: arguments are a JSON-encoded string.
    Str(String),
    /// Non-standard: a few providers inline the arguments as a raw JSON value.
    Value(serde_json::Value),
}

impl ArgumentsWire {
    /// Parses the wire arguments into a [`serde_json::Value`]. An empty or
    /// whitespace-only string (some providers send `""` for a no-arg call)
    /// decodes to an empty object so downstream skills receive a valid object.
    fn into_value(self) -> Result<serde_json::Value, serde_json::Error> {
        match self {
            ArgumentsWire::Str(s) if s.trim().is_empty() => {
                Ok(serde_json::Value::Object(serde_json::Map::new()))
            }
            ArgumentsWire::Str(s) => serde_json::from_str(&s),
            ArgumentsWire::Value(v) => Ok(v),
        }
    }
}

const fn tool_call_kind_function() -> String {
    String::new()
}

impl Serialize for ToolCall {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Emit the standard OpenAI wire shape: arguments as a JSON string.
        let arguments =
            serde_json::to_string(&self.arguments).map_err(serde::ser::Error::custom)?;
        let wire = ToolCallWire {
            id: self.id.clone(),
            kind: "function".to_string(),
            function: ToolCallFunctionWire {
                name: self.name.clone(),
                arguments: ArgumentsWire::Str(arguments),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ToolCallWire::deserialize(deserializer)?;
        let arguments = wire
            .function
            .arguments
            .into_value()
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            id: wire.id,
            name: wire.function.name,
            arguments,
        })
    }
}

impl Serialize for ArgumentsWire {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ArgumentsWire::Str(s) => serializer.serialize_str(s),
            // Round-trip a raw-object form back out as a JSON string so requests
            // we build stay standard-compliant regardless of how we decoded.
            ArgumentsWire::Value(v) => {
                let s = serde_json::to_string(v).map_err(serde::ser::Error::custom)?;
                serializer.serialize_str(&s)
            }
        }
    }
}

/// A tool the model is allowed to call.
///
/// Serialized into the `OpenAI` tools-array shape
/// (`{"type":"function","function":{"name","description","parameters"}}`) when
/// attached to a request. The `input_schema` is a JSON Schema describing the
/// tool's arguments; it becomes the `function.parameters` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    /// Tool name the model uses to invoke this tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's arguments (becomes `function.parameters`).
    pub input_schema: serde_json::Value,
}

impl ToolDefinition {
    /// Validates that this definition is safe to advertise to the model.
    ///
    /// Tool names must match `^[A-Za-z0-9_-]{1,64}$` — a name that works in one
    /// provider but breaks another (or injects unexpected characters) is a
    /// portability and safety hazard, so it is rejected at the boundary rather
    /// than silently sent. The `input_schema` root must be a JSON object (a
    /// scalar/array schema is not a valid `function.parameters` shape).
    ///
    /// # Errors
    /// Returns [`LlmError::InvalidTool`] if the name is malformed or the schema
    /// root is not an object.
    pub fn validate(&self) -> Result<(), LlmError> {
        let name_ok = !self.name.is_empty()
            && self.name.len() <= 64
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !name_ok {
            return Err(LlmError::InvalidTool(format!(
                "tool name {:?} must match ^[A-Za-z0-9_-]{{1,64}}$",
                self.name
            )));
        }
        if !self.input_schema.is_object() {
            return Err(LlmError::InvalidTool(format!(
                "tool {:?} input_schema root must be a JSON object",
                self.name
            )));
        }
        Ok(())
    }
}

/// `OpenAI` tools-array wire shape: `{"type":"function","function":{...}}`.
///
/// Kept private — callers build [`ToolDefinition`]s; this is the serialization
/// envelope the API expects. Borrows from the [`ToolDefinition`] to avoid clones.
#[derive(Serialize)]
struct ToolEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ToolFunction<'a>,
}

#[derive(Serialize)]
struct ToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

impl<'a> From<&'a ToolDefinition> for ToolEnvelope<'a> {
    fn from(def: &'a ToolDefinition) -> Self {
        Self {
            kind: "function",
            function: ToolFunction {
                name: &def.name,
                description: &def.description,
                parameters: &def.input_schema,
            },
        }
    }
}

/// LLM client — stateless HTTP caller for OpenAI-compatible APIs.
///
/// `Clone` is cheap: [`LlmConfig`] is a small owned struct and
/// [`reqwest::Client`] is an `Arc`-backed handle (cloning shares the same
/// connection pool). The failover layer ([`crate::llm_chain::LlmFailover`])
/// clones the active client handle out from under its lock so the HTTP `.await`
/// happens without holding the mutex.
#[derive(Clone)]
pub struct LlmClient {
    config: LlmConfig,
    client: Client,
}

impl LlmClient {
    /// Creates a new LLM client with the given config.
    ///
    /// **F1:** builds the `reqwest::Client` with the **request and connect timeout**
    /// ([`LlmConfig::request_timeout`] / [`LlmConfig::connect_timeout`]). Without a
    /// timeout, a stuck primary (connection accepted but no response ever comes)
    /// would block [`crate::llm_chain::LlmFailover::complete`] forever, and
    /// failover would never trigger. The timeout turns a hung primary into a
    /// **retryable** [`LlmError::Timeout`] error, so the chain moves on to the
    /// fallback.
    ///
    /// If building the `reqwest::Client` fails (unusual — e.g. TLS backend
    /// initialization), this falls back to a default client with no timeouts,
    /// so the constructor stays infallible in its interface (`#[must_use]`, not
    /// `Result`). This is a safe degradation: failover still works on
    /// connection errors, only the hang protection is lost in the extreme case.
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        let client = Client::builder()
            .timeout(config.request_timeout())
            .connect_timeout(config.connect_timeout())
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "reqwest client build with timeouts failed — falling back to default client"
                );
                Client::new()
            });
        Self { config, client }
    }

    /// Returns a reference to the config.
    #[must_use]
    pub const fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Builds the chat completions endpoint URL from the API base.
    ///
    /// Handles trailing slashes correctly and guarantees no whitespace.
    #[must_use]
    pub fn build_endpoint(api_base: &str) -> String {
        format!("{}/chat/completions", api_base.trim_end_matches('/'))
    }

    /// **Failover gap #1, step 1.** Classifies an unsuccessful HTTP response
    /// into the correct [`LlmError`] variant. Called ONLY when
    /// `response.status().is_success()` is `false`. Extracts the `Retry-After` header
    /// (for 429s), redacts the body on auth errors (401/403) to prevent
    /// key leaks, and delegates classification to the pure [`LlmError::from_status`],
    /// which is directly unit-testable without a network.
    ///
    /// **404 is redacted too, and never even read:** a retired-model 404
    /// body has been observed (production incident) to embed an account
    /// identifier. The status code alone is sufficient to classify
    /// [`LlmError::NotFound`], so the body is never fetched for a 404 —
    /// nothing to leak. A 400 body IS still read (needed to detect the
    /// "model not found" phrasing some providers use instead of a real 404),
    /// but its classification only ever surfaces the status code
    /// downstream — see [`LlmError::redacted_status_line`].
    async fn error_from_response(response: reqwest::Response) -> LlmError {
        let status = response.status().as_u16();
        let retry_after = LlmError::parse_retry_after(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
        );
        // Redact body for auth errors + 404 to prevent key/account-id leakage
        // in logs (401/403: key leak; 404: observed account-id leak).
        let is_sensitive = matches!(status, 401 | 403 | 404);
        let detail = if is_sensitive {
            "[redacted]".to_string()
        } else {
            response.text().await.unwrap_or_default()
        };
        LlmError::from_status(status, &detail, retry_after)
    }

    /// Completes a chat conversation, returning the response text.
    ///
    /// **Auto-continuation on `finish_reason == "length"`:** a single
    /// completion call can be cut off mid-sentence by the `max_tokens` limit.
    /// When that happens, this appends the partial reply plus a "continue"
    /// nudge ([`CONTINUATION_NUDGE`]) and calls again, concatenating the
    /// result — bounded by [`max_continuations`] rounds (env
    /// `FAMILYCLAW_MAX_CONTINUATIONS`, default [`DEFAULT_MAX_CONTINUATIONS`],
    /// `0` disables) and by [`MAX_CONTINUATION_OUTPUT_CHARS`] total
    /// accumulated size. If a continuation call itself fails, the
    /// **already-accumulated** partial text is returned as `Ok` rather than
    /// discarding it — losing a partial reply is worse than returning one
    /// that ends slightly early. Tool-call responses
    /// ([`FinishReason::ToolCalls`]) are never produced on this path (this
    /// method never advertises tools — see [`Self::complete_with_tools`]) so
    /// they are unaffected by construction.
    ///
    /// # Errors
    /// Returns an error if the *first* HTTP request fails or its response is
    /// invalid. Failures on continuation rounds do not surface as `Err` (see
    /// above) — they end the loop and return the partial text accumulated so far.
    pub async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
        let (mut text, mut finish_reason) = self.complete_once(messages).await?;

        let max_rounds = max_continuations();
        let mut round: u32 = 0;
        while finish_reason.is_length()
            && round < max_rounds
            && text.len() < MAX_CONTINUATION_OUTPUT_CHARS
        {
            round += 1;
            let mut continuation_messages = messages.to_vec();
            continuation_messages.push(LlmMessage::assistant(text.clone()));
            continuation_messages.push(LlmMessage::user(CONTINUATION_NUDGE));

            match self.complete_once(&continuation_messages).await {
                Ok((more, next_finish_reason)) => {
                    text.push_str(&more);
                    finish_reason = next_finish_reason;
                    tracing::info!(
                        round,
                        accumulated_chars = text.len(),
                        "llm: auto-continuation round completed (finish_reason=length)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        round,
                        accumulated_chars = text.len(),
                        error = %e,
                        "llm: continuation call failed — returning accumulated partial text instead of losing it"
                    );
                    break;
                }
            }
        }

        if text.len() > MAX_CONTINUATION_OUTPUT_CHARS {
            let cut = safe_truncate_to(&text, MAX_CONTINUATION_OUTPUT_CHARS).len();
            text.truncate(cut);
        }

        Ok(text)
    }

    /// One raw completion call: sends `messages` (no tools) and returns the
    /// response text plus its [`FinishReason`]. Shared by [`Self::complete`]'s
    /// first call and each of its continuation rounds.
    ///
    /// Dispatches on [`LlmConfig::wire_format`] — this is the ANY-AI provider
    /// seam. [`LlmWireFormat::AnthropicMessages`] / [`LlmWireFormat::Bedrock`]
    /// are not implemented yet (design-doc only): calling either returns
    /// [`LlmError::Http`] instead of silently talking `OpenAI`-shaped wire to
    /// an incompatible endpoint.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails, the response is invalid,
    /// or the wire format is not implemented.
    async fn complete_once(
        &self,
        messages: &[LlmMessage],
    ) -> Result<(String, FinishReason), LlmError> {
        match self.config.wire_format {
            LlmWireFormat::OpenAiChat => self.complete_once_openai(messages).await,
            LlmWireFormat::GeminiGenerate => self.complete_once_gemini(messages).await,
            LlmWireFormat::AnthropicMessages | LlmWireFormat::Bedrock => {
                Err(Self::unimplemented_wire_format_error(self.config.wire_format))
            }
        }
    }

    /// The error returned for a [`LlmWireFormat`] that has no implementation
    /// yet ([`LlmWireFormat::AnthropicMessages`], [`LlmWireFormat::Bedrock`],
    /// and — outside [`Self::complete`] — [`LlmWireFormat::GeminiGenerate`]
    /// with tools/streaming). Classified as [`LlmError::Http`]: it is a
    /// deterministic config error for THIS entry, but (unlike
    /// [`LlmError::Parse`]/[`LlmError::InvalidTool`]) still retryable at the
    /// chain level, since a different chain entry may use a wire format that
    /// IS implemented.
    fn unimplemented_wire_format_error(format: LlmWireFormat) -> LlmError {
        LlmError::Http(format!(
            "wire format '{}' is not implemented yet — see docs/design/multi-provider-wire-formats.md",
            format.as_word()
        ))
    }

    /// `OpenAI`-compatible chat-completions call (the original implementation,
    /// unchanged in behavior). See [`Self::complete_once`].
    async fn complete_once_openai(
        &self,
        messages: &[LlmMessage],
    ) -> Result<(String, FinishReason), LlmError> {
        let endpoint = Self::build_endpoint(&self.config.api_base);

        let request_body = ChatCompletionsRequest {
            model: &self.config.model,
            messages,
            max_tokens: self.config.max_tokens,
            // Empty tools + no tool_choice → these fields are skipped, so this
            // request serializes byte-identically to the pre-tools version.
            tools: Vec::new(),
            tool_choice: None,
        };

        let response = self
            .client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest("request failed", &e))?;

        if !response.status().is_success() {
            return Err(Self::error_from_response(response).await);
        }

        let chat_response: ChatCompletionsResponse = response.json().await.map_err(|e| {
            // Body-read timeout (the whole-request budget was exceeded while
            // reading the response) -> retryable Timeout (F1); a genuine decode error -> Parse.
            if e.is_timeout() {
                LlmError::Timeout(format!("response read timed out: {e}"))
            } else {
                LlmError::Parse(format!("response parse error: {e}"))
            }
        })?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            // Empty choices = the model produced nothing this turn. That is
            // RETRYABLE (another model in the chain may produce content), so
            // classify it as NoContent — NOT Parse (which is terminal and
            // would defeat failover). [F1 invariant: see LlmError::is_retryable]
            .ok_or(LlmError::NoContent)?;

        let finish_reason = FinishReason::from_wire(choice.finish_reason.as_deref());
        let content = choice.message.content.ok_or(LlmError::NoContent)?;
        Ok((content, finish_reason))
    }

    /// Builds the Gemini `generateContent` endpoint URL (without the `key`
    /// query parameter — that is attached separately via `.query(...)` so
    /// `reqwest` handles URL-encoding of the API key, rather than
    /// hand-formatting it into the URL string).
    ///
    /// `api_base` is expected to be the Gemini API root (e.g.
    /// `https://generativelanguage.googleapis.com/v1beta`), analogous to how
    /// [`Self::build_endpoint`] expects an `OpenAI`-compatible root.
    #[must_use]
    fn gemini_endpoint(api_base: &str, model: &str) -> String {
        format!(
            "{}/models/{model}:generateContent",
            api_base.trim_end_matches('/')
        )
    }

    /// Native Google Gemini `generateContent` call (verified minimal slice of
    /// the ANY-AI wire-format layer — see module docs). Sends `messages` with
    /// no tools/functions attached and returns the response text plus its
    /// [`FinishReason`] (mapped through [`FinishReason::from_gemini_wire`]).
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    async fn complete_once_gemini(
        &self,
        messages: &[LlmMessage],
    ) -> Result<(String, FinishReason), LlmError> {
        let endpoint = Self::gemini_endpoint(&self.config.api_base, &self.config.model);
        let request_body = GeminiGenerateContentRequest::from_messages(messages, self.config.max_tokens);

        let response = self
            .client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .query(&[("key", self.config.api_key.as_str())])
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest("request failed", &e))?;

        if !response.status().is_success() {
            return Err(Self::error_from_response(response).await);
        }

        let gemini_response: GeminiGenerateContentResponse = response.json().await.map_err(|e| {
            if e.is_timeout() {
                LlmError::Timeout(format!("response read timed out: {e}"))
            } else {
                LlmError::Parse(format!("response parse error: {e}"))
            }
        })?;

        // Empty candidates = the model produced nothing this turn — RETRYABLE
        // (another model in the chain may produce content), same invariant as
        // the OpenAI path's empty `choices` (see complete_once_openai).
        let candidate = gemini_response
            .candidates
            .into_iter()
            .next()
            .ok_or(LlmError::NoContent)?;

        let finish_reason = FinishReason::from_gemini_wire(candidate.finish_reason.as_deref());
        let content = candidate
            .content
            .map(|c| c.parts.into_iter().filter_map(|p| p.text).collect::<String>())
            .filter(|s| !s.is_empty())
            .ok_or(LlmError::NoContent)?;

        Ok((content, finish_reason))
    }

    /// Opens an SSE stream (`stream: true`) and returns a stream of text chunks.
    ///
    /// **Wire format:** only [`LlmWireFormat::OpenAiChat`] is implemented.
    /// Every other wire format returns [`LlmError::Http`] immediately (see
    /// module docs) rather than silently sending `OpenAI`-shaped streaming
    /// requests to an incompatible endpoint.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails before the stream is
    /// opened, or the wire format does not support streaming.
    pub async fn complete_stream(
        &self,
        messages: &[LlmMessage],
    ) -> std::result::Result<LlmChunkStream, LlmError> {
        if self.config.wire_format != LlmWireFormat::OpenAiChat {
            return Err(Self::unimplemented_wire_format_error(self.config.wire_format));
        }
        let endpoint = Self::build_endpoint(&self.config.api_base);
        let request_body = ChatCompletionsStreamRequest {
            model: &self.config.model,
            messages,
            max_tokens: self.config.max_tokens,
            stream: true,
            tools: Vec::new(),
            tool_choice: None,
        };

        let response = self
            .client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest("stream request failed", &e))?;

        if !response.status().is_success() {
            return Err(Self::error_from_response(response).await);
        }

        let byte_stream = response.bytes_stream();
        let stream = async_stream::stream! {
            let mut buffer = String::new();
            futures_util::pin_mut!(byte_stream);
            while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim_end_matches('\r').to_string();
                            buffer = buffer[line_end + 1..].to_string();
                            if let Some(delta) = parse_sse_delta_line(&line) {
                                yield Ok(delta);
                            }
                        }
                    }
                    Err(e) => {
                        yield Err(LlmError::from_reqwest("stream read failed", &e));
                        break;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }

    /// Completes a chat conversation, advertising the given `tools`, and returns
    /// both text and any tool calls the model chose to make.
    ///
    /// When `tools` is empty this advertises no tools (and the wire request is
    /// identical to [`Self::complete`] plus the tool-aware response parse).
    /// `tool_choice` is `"auto"` by default when tools are present; callers may
    /// pass `"required"` to force at least one tool call (action-request escalation).
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    pub async fn complete_with_tools(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> Result<CompletionResult, LlmError> {
        self.complete_with_tools_choice(messages, tools, None).await
    }

    /// Like [`Self::complete_with_tools`] with explicit `OpenAI` `tool_choice`.
    ///
    /// **Wire format:** [`LlmWireFormat::OpenAiChat`] is fully implemented
    /// (text + tool calls). [`LlmWireFormat::GeminiGenerate`] supports the
    /// **tool-less** path only (`tools` empty) — it delegates to
    /// [`Self::complete_once_gemini`] and wraps the text into a
    /// [`CompletionResult`] with no tool calls; a non-empty `tools` list
    /// returns [`LlmError::Http`] (Gemini function calling is not
    /// implemented in this slice — see module docs). Every other wire format
    /// always returns [`LlmError::Http`].
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails, the response is invalid,
    /// or the wire format/tool combination is not implemented.
    pub async fn complete_with_tools_choice(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
    ) -> Result<CompletionResult, LlmError> {
        // Validate every tool BEFORE sending — a malformed name/schema is a
        // deterministic config error, caught at the boundary, not on the wire.
        for tool in tools {
            tool.validate()?;
        }

        match self.config.wire_format {
            LlmWireFormat::OpenAiChat => {
                self.complete_with_tools_choice_openai(messages, tools, tool_choice)
                    .await
            }
            LlmWireFormat::GeminiGenerate if tools.is_empty() => {
                let (text, _finish_reason) = self.complete_once_gemini(messages).await?;
                Ok(CompletionResult {
                    content: Some(text),
                    tool_calls: None,
                })
            }
            other => Err(Self::unimplemented_wire_format_error(other)),
        }
    }

    /// `OpenAI`-compatible tool-calling completion (the original
    /// implementation, unchanged in behavior). See
    /// [`Self::complete_with_tools_choice`].
    async fn complete_with_tools_choice_openai(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
    ) -> Result<CompletionResult, LlmError> {
        let endpoint = Self::build_endpoint(&self.config.api_base);

        let request_body = ChatCompletionsRequest {
            model: &self.config.model,
            messages,
            max_tokens: self.config.max_tokens,
            tools: tools.iter().map(ToolEnvelope::from).collect(),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some(match tool_choice {
                    Some("required") => "required",
                    _ => "auto",
                })
            },
        };

        let response = self
            .client
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&request_body)
            .send()
            .await
            .map_err(|e| LlmError::from_reqwest("request failed", &e))?;

        if !response.status().is_success() {
            return Err(Self::error_from_response(response).await);
        }

        let chat_response: ChatCompletionsResponse = response.json().await.map_err(|e| {
            // Body-read timeout (the whole-request budget was exceeded while
            // reading the response) -> retryable Timeout (F1); a genuine decode error -> Parse.
            if e.is_timeout() {
                LlmError::Timeout(format!("response read timed out: {e}"))
            } else {
                LlmError::Parse(format!("response parse error: {e}"))
            }
        })?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            // Empty choices = retryable NoContent (another model may answer),
            // NOT terminal Parse — keeps failover working. [F1 invariant]
            .ok_or(LlmError::NoContent)?;

        let content = choice.message.content;
        let tool_calls = choice.message.tool_calls;

        // A choice with NEITHER content NOR tool calls is a silent "succeeded
        // but did nothing" state. For the tool loop (1B) that is a trap — the
        // loop has no text to reply with and no call to dispatch. Classify it
        // as retryable NoContent so failover tries the next model instead of
        // surfacing an empty result. [F1 invariant — mirrors `complete`.]
        let has_tool_calls = tool_calls.as_ref().is_some_and(|c| !c.is_empty());
        if content.is_none() && !has_tool_calls {
            return Err(LlmError::NoContent);
        }

        Ok(CompletionResult {
            content,
            tool_calls,
        })
    }
}

/// Result of an LLM completion.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    /// Text content (if any)
    pub content: Option<String>,
    /// Tool calls (if any)
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl CompletionResult {
    /// Returns true if this result has tool calls.
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Returns the text content or an empty string.
    #[must_use]
    pub fn text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// A stream of streamed text chunks ([`LlmClient::complete_stream`]).
pub type LlmChunkStream = Pin<Box<dyn Stream<Item = std::result::Result<String, LlmError>> + Send>>;

/// LLM error types.
///
/// **Failover gap #1, step 1 — error taxonomy.** Previously *all* unsuccessful
/// HTTP statuses collapsed into a single [`LlmError::Http`] class, so 429
/// (rate limit), 401/403 (auth/billing), and 503/529 (overloaded) could not
/// be distinguished from one another — a differentiated backoff/rotation was structurally
/// impossible. These variants *separate out* the cases critical to failover,
/// so that a future cooldown/key-rotation layer can branch on them
/// ([`LlmError::cooldown_hint`] provides the seed). This step does not yet build
/// the backoff state machine itself — only the taxonomy and its propagation.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// HTTP request failed (a connection error or another unsuccessful status code that
    /// does not belong to a more specific class: a generic 4xx/5xx, ECONNREFUSED, etc.).
    Http(String),
    /// The request timed out (request or connect timeout). **F1:** this is
    /// **retryable** — a stuck primary gives up via the timeout, and
    /// [`crate::llm_chain::LlmFailover`] moves on to the next fallback.
    Timeout(String),
    /// HTTP 429 — the provider rate-limited this key/model. Retryable, but
    /// **intentionally separated** from [`LlmError::Http`]: a future backoff layer
    /// treats this differently (wait `retry_after` seconds and/or rotate the
    /// key pool). `retry_after` is extracted from the `Retry-After` header (in seconds)
    /// if the provider supplies it. [Failover gap #1, step 1]
    RateLimited {
        /// Provider message / context (for logs).
        message: String,
        /// The `Retry-After` header's value in seconds, if the provider supplied it.
        retry_after: Option<u64>,
    },
    /// HTTP 401/403 — the key is invalid, expired, or billing has
    /// run out. **A key-pool rotation signal, NOT a model-fallback signal:**
    /// the same key fails on every model, so a future layer swaps the
    /// key (not the model). The body is redacted to prevent leaks.
    /// [Failover gap #1, step 1]
    AuthFailed(String),
    /// HTTP 503/529 — the provider is momentarily overloaded. Retryable
    /// with backoff; **separated** from [`LlmError::Http`] so that a future layer can
    /// wait with an escalating delay on the same provider instead of slamming
    /// through the whole chain. [Failover gap #1, step 1]
    Overloaded(String),
    /// HTTP 404 — or a 400 whose body says the model/function id does not
    /// exist upstream (some OpenAI-compatible providers, e.g. NVIDIA NIM,
    /// return 400 instead of 404 for a retired model). **Provider-dead
    /// signal, distinct from a transient outage:** the model itself has
    /// been retired/renamed, so retrying the SAME model (even with a
    /// different key) will never succeed. The failover layer
    /// ([`crate::llm_chain::LlmFailover`]) rotates to the next entry
    /// immediately (no key retry — the key isn't the problem) and cools
    /// this entry down on the **long** (auth-style) ladder, since a retired
    /// model will not come back within the short general ladder's 60s.
    /// [Failover gap #1 — production incident: retired NIM model → 404 on
    /// every call, chain did not rotate.]
    NotFound(String),
    /// Response parsing failed
    Parse(String),
    /// No content in response
    NoContent,
    /// A tool definition advertised to the model is malformed (bad name or
    /// non-object schema). Deterministic config error → **not** retryable.
    InvalidTool(String),
}

/// One-word, **non-sensitive** classification of an [`LlmError`] — safe to
/// surface in a user-facing reply or a log line (never carries the raw
/// provider response body, which can contain account identifiers — see the
/// retired-model incident this exists for). Used both by the cooldown/
/// rotation layer ([`crate::llm_chain::LlmFailover`]) and by the
/// "why did this turn fail" messaging (`familyclaw_agent::agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmFailureClass {
    /// HTTP 429.
    RateLimited,
    /// HTTP 401/403.
    AuthFailed,
    /// HTTP 503/529.
    Overloaded,
    /// HTTP 404 (or a 400-shaped "model not found").
    ProviderNotFound,
    /// Request/connect timeout.
    Timeout,
    /// Generic HTTP/connection error.
    Http,
    /// The model produced no content/tool calls.
    NoContent,
    /// Response body failed to parse.
    Parse,
    /// A tool definition was malformed.
    InvalidTool,
}

impl LlmFailureClass {
    /// The one-word tag (stable — used both in logs and parsed back out of
    /// the tagged error string in `familyclaw_agent::agent`).
    #[must_use]
    pub const fn as_word(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::AuthFailed => "auth_failed",
            Self::Overloaded => "overloaded",
            Self::ProviderNotFound => "provider_not_found",
            Self::Timeout => "timeout",
            Self::Http => "http_error",
            Self::NoContent => "no_content",
            Self::Parse => "parse_error",
            Self::InvalidTool => "invalid_tool",
        }
    }
}

impl LlmError {
    /// Is the error **retryable** (may failover try the next
    /// client)?
    ///
    /// **F1 core:** [`LlmError::Timeout`] (a stuck/hung primary) is
    /// retryable, as is [`LlmError::Http`] (a connection error or a 5xx/429-type
    /// transient disruption) and [`LlmError::NoContent`] (another model may produce
    /// content). Only [`LlmError::Parse`] is **not** retryable: the same response
    /// would parse into the same error again on the next attempt with the same model,
    /// so it is deterministic — but because failover tries *different*
    /// clients (a different model/endpoint), a parse error on one model doesn't say
    /// anything about the next. Conservatively: treat a parse error as
    /// **non-retryable**, so an obviously broken request (e.g. the wrong
    /// request shape) doesn't grind the whole chain for nothing; the network/timeout/content
    /// classes are retryable.
    /// **Failover gap #1, step 1 — taxonomy propagation:** the new variants
    /// [`LlmError::RateLimited`], [`LlmError::AuthFailed`], and
    /// [`LlmError::Overloaded`] are ALL currently retryable, so that
    /// the chain proceeds *today* exactly as before (none of these are
    /// terminal). The difference is that the variants are now **distinct**, so
    /// a future cooldown/key-rotation layer can branch on them:
    /// `RateLimited` -> wait `retry_after` ([`Self::cooldown_hint`]),
    /// `AuthFailed` -> rotate the key (not the model), `Overloaded` -> escalating
    /// backoff. Only [`LlmError::Parse`] and [`LlmError::InvalidTool`] are
    /// deterministically non-retryable.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            LlmError::Timeout(_)
            | LlmError::Http(_)
            | LlmError::NoContent
            // RateLimited/AuthFailed/Overloaded: distinct variants, but today
            // still retryable so the chain proceeds (a cooldown layer comes later).
            | LlmError::RateLimited { .. }
            | LlmError::AuthFailed(_)
            | LlmError::Overloaded(_)
            // NotFound (provider-dead): retryable at the CHAIN level — a
            // different model/provider entirely may still answer, and PASS 2
            // (last resort) gives a transient false-404 one chance to
            // self-heal. It is only "terminal" for THIS entry, which the
            // cooldown layer expresses via a long cooldown, not via
            // is_retryable.
            | LlmError::NotFound(_) => true,
            // Parse + InvalidTool are deterministic: the same request would fail
            // identically, so do not grind the whole chain.
            LlmError::Parse(_) | LlmError::InvalidTool(_) => false,
        }
    }

    /// **Failover gap #1, step 1 — seed for a future backoff state machine.**
    /// Returns the *suggested* wait time before it's worth retrying the same
    /// provider/key. This is a PURE hint (does not sleep, does not mutate state): a future
    /// cooldown/rotation layer will consume it — the current [`LlmFailover`] does not yet
    /// use it, so behavior does not change.
    ///
    /// - [`LlmError::RateLimited`] -> the `Retry-After` value if the provider supplied it
    ///   (in seconds), otherwise the default [`Self::DEFAULT_RATE_LIMIT_COOLDOWN`].
    /// - [`LlmError::Overloaded`] -> the default [`Self::DEFAULT_OVERLOAD_COOLDOWN`]
    ///   (the provider is recovering — wait before returning to the same provider).
    /// - Everything else -> `None` (no natural cooldown; failover switches
    ///   clients immediately).
    ///
    /// [`LlmFailover`]: crate::llm_chain::LlmFailover
    #[must_use]
    pub fn cooldown_hint(&self) -> Option<Duration> {
        match self {
            LlmError::RateLimited { retry_after, .. } => {
                Some(retry_after.map_or(Self::DEFAULT_RATE_LIMIT_COOLDOWN, Duration::from_secs))
            }
            LlmError::Overloaded(_) => Some(Self::DEFAULT_OVERLOAD_COOLDOWN),
            LlmError::Http(_)
            | LlmError::Timeout(_)
            | LlmError::AuthFailed(_)
            // NotFound has no short "hint": the cooldown/rotation layer
            // (llm_chain.rs) puts it straight on the long auth-style ladder
            // instead of consulting this hint — a retired model won't come
            // back in seconds.
            | LlmError::NotFound(_)
            | LlmError::Parse(_)
            | LlmError::NoContent
            | LlmError::InvalidTool(_) => None,
        }
    }

    /// Default cooldown for 429 when the provider does NOT give a `Retry-After` header.
    /// A moderate 5s — a starting value for a future backoff layer, not final.
    pub const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(5);

    /// Default cooldown for the 503/529 (overloaded) case. 2s: the provider is
    /// alive but congested, so a shorter wait than for rate-limit is enough.
    pub const DEFAULT_OVERLOAD_COOLDOWN: Duration = Duration::from_secs(2);

    /// **Failover gap #1, step 1 — testable seam.** Maps an unsuccessful
    /// HTTP status code to the correct [`LlmError`] variant. `detail` is already
    /// (redacted, in the auth case) the body/context message; `retry_after` is the
    /// second count parsed from the `Retry-After` header if present.
    ///
    /// - 429 -> [`LlmError::RateLimited`] (with `retry_after`).
    /// - 401/403 -> [`LlmError::AuthFailed`] (key rotation signal).
    /// - 503/529 -> [`LlmError::Overloaded`] (provider overloaded).
    /// - 404, or 400 whose body reads like "model not found" ->
    ///   [`LlmError::NotFound`] (provider-dead signal — see its doc comment).
    /// - other -> [`LlmError::Http`].
    #[must_use]
    fn from_status(status: u16, detail: &str, retry_after: Option<u64>) -> Self {
        match status {
            429 => LlmError::RateLimited {
                message: format!("HTTP 429: {detail}"),
                retry_after,
            },
            401 | 403 => LlmError::AuthFailed(format!("HTTP {status}: {detail}")),
            503 | 529 => LlmError::Overloaded(format!("HTTP {status}: {detail}")),
            404 => LlmError::NotFound(format!("HTTP 404: {detail}")),
            400 if Self::looks_like_model_not_found(detail) => {
                LlmError::NotFound(format!("HTTP 400: {detail}"))
            }
            _ => LlmError::Http(format!("HTTP {status}: {detail}")),
        }
    }

    /// Heuristic for the "model retired upstream" case some OpenAI-compatible
    /// providers report as a 400 rather than a 404 (production incident:
    /// NVIDIA NIM returns `400 {"...Function id ... not found..."}` for a
    /// retired model id). Deliberately conservative — requires BOTH a
    /// model/function-id mention AND a "not found"/"does not exist" phrase,
    /// so an unrelated validation 400 (bad JSON, bad parameter) is not
    /// misclassified as provider-dead.
    #[must_use]
    fn looks_like_model_not_found(detail: &str) -> bool {
        let lower = detail.to_ascii_lowercase();
        let mentions_model = lower.contains("model") || lower.contains("function id");
        let mentions_missing = lower.contains("not found")
            || lower.contains("not_found")
            || lower.contains("does not exist")
            || lower.contains("unknown model");
        mentions_model && mentions_missing
    }

    /// Extracts the leading `HTTP <code>` prefix that this crate always puts
    /// at the start of status-derived error messages (see [`Self::from_status`]).
    /// `None` for error kinds that don't carry an HTTP status (e.g.
    /// [`LlmError::Timeout`], [`LlmError::Parse`]) or whose message wasn't
    /// built from `from_status` (e.g. [`LlmError::from_reqwest`]'s
    /// non-"HTTP "-prefixed messages).
    #[must_use]
    fn http_status_code(&self) -> Option<u16> {
        let msg: &str = match self {
            LlmError::NotFound(m)
            | LlmError::AuthFailed(m)
            | LlmError::Overloaded(m)
            | LlmError::Http(m) => m,
            LlmError::RateLimited { message, .. } => message,
            LlmError::Timeout(_)
            | LlmError::Parse(_)
            | LlmError::NoContent
            | LlmError::InvalidTool(_) => return None,
        };
        let rest = msg.strip_prefix("HTTP ")?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    /// This [`LlmError`]'s [`LlmFailureClass`].
    #[must_use]
    pub const fn failure_class(&self) -> LlmFailureClass {
        match self {
            LlmError::RateLimited { .. } => LlmFailureClass::RateLimited,
            LlmError::AuthFailed(_) => LlmFailureClass::AuthFailed,
            LlmError::Overloaded(_) => LlmFailureClass::Overloaded,
            LlmError::NotFound(_) => LlmFailureClass::ProviderNotFound,
            LlmError::Timeout(_) => LlmFailureClass::Timeout,
            LlmError::Http(_) => LlmFailureClass::Http,
            LlmError::NoContent => LlmFailureClass::NoContent,
            LlmError::Parse(_) => LlmFailureClass::Parse,
            LlmError::InvalidTool(_) => LlmFailureClass::InvalidTool,
        }
    }

    /// **Redacted status line** — `"HTTP <code>"` if the error carries a
    /// status, otherwise the failure class's one word (e.g. `"timeout"`).
    /// This is the ONLY representation of the error that a user-facing
    /// message may include: it is built purely from the status code this
    /// crate itself parsed off the wire, never from the provider's response
    /// body (which can embed account identifiers — the incident this exists
    /// for leaked one via a raw error body reaching a log/reply).
    #[must_use]
    pub fn redacted_status_line(&self) -> String {
        match self.http_status_code() {
            Some(code) => format!("HTTP {code}"),
            None => self.failure_class().as_word().to_string(),
        }
    }

    /// Parses the `Retry-After` header into **seconds**. OpenAI-compatible
    /// providers typically send an integer (delta-seconds); the HTTP-date
    /// format is NOT supported here (returns `None`, so the default cooldown takes
    /// effect). A pure function -> directly testable.
    #[must_use]
    fn parse_retry_after(value: Option<&str>) -> Option<u64> {
        value?.trim().parse::<u64>().ok()
    }

    /// Maps a `reqwest::Error` to the correct [`LlmError`] class: a genuine timeout ->
    /// [`LlmError::Timeout`] (retryable, F1); everything else (incl. ECONNREFUSED
    /// `is_connect()`) -> [`LlmError::Http`], which is likewise retryable.
    /// This way both a stuck primary (timeout) and a dead primary
    /// (connection error) trigger failover, but the error classes stay distinct
    /// in the logs and in [`is_retryable`](LlmError::is_retryable)'s semantics.
    /// `context` prefixes the message (e.g. `"request failed"`).
    #[must_use]
    fn from_reqwest(context: &str, e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            LlmError::Timeout(format!("{context}: {e}"))
        } else {
            LlmError::Http(format!("{context}: {e}"))
        }
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(msg) => write!(f, "HTTP error: {msg}"),
            LlmError::Timeout(msg) => write!(f, "Timeout error: {msg}"),
            LlmError::RateLimited {
                message,
                retry_after,
            } => match retry_after {
                Some(s) => write!(f, "Rate limited (retry after {s}s): {message}"),
                None => write!(f, "Rate limited: {message}"),
            },
            LlmError::AuthFailed(msg) => write!(f, "Auth failed: {msg}"),
            LlmError::Overloaded(msg) => write!(f, "Provider overloaded: {msg}"),
            LlmError::NotFound(msg) => write!(f, "Model/provider not found: {msg}"),
            LlmError::Parse(msg) => write!(f, "Parse error: {msg}"),
            LlmError::NoContent => write!(f, "No content in response"),
            LlmError::InvalidTool(msg) => write!(f, "Invalid tool definition: {msg}"),
        }
    }
}

impl std::error::Error for LlmError {}

// Internal request/response structs for the OpenAI API

#[derive(Serialize)]
struct ChatCompletionsStreamRequest<'a, 'b> {
    model: &'a str,
    messages: &'b [LlmMessage],
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolEnvelope<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

#[derive(Serialize)]
struct ChatCompletionsRequest<'a, 'b> {
    model: &'a str,
    messages: &'b [LlmMessage],
    max_tokens: u32,
    /// Tools the model may call. Absent (`skip_serializing_if`) when empty, so
    /// existing tool-less requests serialize byte-identically to before.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolEnvelope<'a>>,
    /// Tool-choice hint (e.g. `"auto"`). Absent when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    /// `"stop" | "length" | "tool_calls" | ...`. `#[serde(default)]` tolerates
    /// providers/fixtures that omit it (-> `None` -> [`FinishReason::Stop`]
    /// via [`FinishReason::from_wire`]).
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

/// Extracts one SSE `data:` line's delta content. Returns `None` for `[DONE]` lines.
fn parse_sse_delta_line(line: &str) -> Option<String> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload == "[DONE]" {
        return None;
    }
    let chunk: StreamChunk = serde_json::from_str(payload).ok()?;
    chunk
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.delta.content)
        .filter(|s| !s.is_empty())
}

// ── Gemini `generateContent` wire shapes (ANY-AI provider support) ─────────
//
// Native Google Gemini wire format, distinct from the OpenAI-compatible
// shape above. Field names follow Gemini's `camelCase` convention
// (`generationConfig`, `maxOutputTokens`, `systemInstruction`,
// `finishReason`) via `#[serde(rename_all = "camelCase")]`.

/// `POST {api_base}/models/{model}:generateContent` request body.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerateContentRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiSystemInstruction>,
    generation_config: GeminiGenerationConfig,
}

impl GeminiGenerateContentRequest {
    /// Converts the provider-agnostic [`LlmMessage`] list into Gemini's
    /// `contents` + `systemInstruction` shape.
    ///
    /// - [`LlmRole::System`] messages are extracted and concatenated (joined
    ///   by a blank line) into `systemInstruction` — Gemini has no
    ///   `"system"` role inside `contents`.
    /// - [`LlmRole::User`] and [`LlmRole::Tool`] map to Gemini's `"user"`
    ///   role — Gemini's native tool-result shape
    ///   (`functionResponse`/`functionCall`) is NOT implemented in this
    ///   slice (this wire format only handles the tool-less path — see
    ///   [`LlmClient::complete_with_tools_choice`]), so a [`LlmRole::Tool`]
    ///   message (which cannot occur on that path today) still degrades
    ///   safely to a plain text turn rather than being dropped or panicking.
    /// - [`LlmRole::Assistant`] maps to Gemini's `"model"` role.
    fn from_messages(messages: &[LlmMessage], max_tokens: u32) -> Self {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut contents = Vec::with_capacity(messages.len());
        for m in messages {
            match m.role {
                LlmRole::System => {
                    if !m.content.is_empty() {
                        system_parts.push(&m.content);
                    }
                }
                LlmRole::User | LlmRole::Tool => contents.push(GeminiContent {
                    role: "user".to_string(),
                    parts: vec![GeminiPart {
                        text: Some(m.content.clone()),
                    }],
                }),
                LlmRole::Assistant => contents.push(GeminiContent {
                    role: "model".to_string(),
                    parts: vec![GeminiPart {
                        text: Some(m.content.clone()),
                    }],
                }),
            }
        }
        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(GeminiSystemInstruction {
                parts: vec![GeminiPart {
                    text: Some(system_parts.join("\n\n")),
                }],
            })
        };
        Self {
            contents,
            system_instruction,
            generation_config: GeminiGenerationConfig {
                max_output_tokens: max_tokens,
            },
        }
    }
}

#[derive(Debug, Serialize, PartialEq)]
struct GeminiSystemInstruction {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    max_output_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct GeminiPart {
    /// `#[serde(default)]`: a part with no `text` (e.g. an inline-data /
    /// function-call part this slice doesn't model) still decodes instead
    /// of failing the whole response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

/// `generateContent` response body.
#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    /// `"STOP" | "MAX_TOKENS" | ...`. `#[serde(default)]` tolerates
    /// responses that omit it (-> `None` -> [`FinishReason::Stop`] via
    /// [`FinishReason::from_gemini_wire`]).
    #[serde(default)]
    finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_config_creation() {
        let config = LlmConfig::new("https://api.openai.com/v1", "sk-test123", "gpt-4o");
        assert_eq!(config.api_base, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(config.max_tokens, 4096);

        let config_with_tokens =
            LlmConfig::new("https://api.openai.com/v1", "key", "model").with_max_tokens(4096);
        assert_eq!(config_with_tokens.max_tokens, 4096);
    }

    #[test]
    fn test_llm_message_creation() {
        let system = LlmMessage::system("You are helpful.");
        assert_eq!(system.role, LlmRole::System);
        assert_eq!(system.content, "You are helpful.");

        let user = LlmMessage::user("Hello!");
        assert_eq!(user.role, LlmRole::User);
        assert_eq!(user.content, "Hello!");

        let assistant = LlmMessage::assistant("Hi there!");
        assert_eq!(assistant.role, LlmRole::Assistant);

        let tool = LlmMessage::tool_result("call_123", "result data");
        assert_eq!(tool.role, LlmRole::Tool);
        assert_eq!(tool.tool_call_id, Some("call_123".into()));
    }

    #[test]
    fn test_llm_message_tool_calls() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "search".into(),
            arguments: serde_json::json!({"q": "test"}),
        }];

        let msg = LlmMessage::assistant("Let me check.").with_tool_calls(calls.clone());
        assert!(msg.has_tool_calls());
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_completion_result() {
        let result_text = CompletionResult {
            content: Some("hello".into()),
            tool_calls: None,
        };
        assert_eq!(result_text.text(), "hello");
        assert!(!result_text.has_tool_calls());

        let result_tools = CompletionResult {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "c1".into(),
                name: "test".into(),
                arguments: serde_json::json!({}),
            }]),
        };
        assert!(result_tools.has_tool_calls());
        assert_eq!(result_tools.text(), "");
    }

    #[test]
    fn test_serde_roundtrip() {
        let config = LlmConfig::new("http://test", "key", "model");
        let json = serde_json::to_string(&config).unwrap();
        let back: LlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.api_base, back.api_base);
        assert_eq!(config.model, back.model);

        let msg = LlmMessage::user("test content");
        let json = serde_json::to_string(&msg).unwrap();
        let back: LlmMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.role, back.role);
        assert_eq!(msg.content, back.content);
    }

    #[test]
    fn parse_sse_delta_line_extracts_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#;
        assert_eq!(parse_sse_delta_line(line).as_deref(), Some("hello"));
        assert_eq!(parse_sse_delta_line("data: [DONE]"), None);
        assert_eq!(parse_sse_delta_line("event: ping"), None);
        assert_eq!(parse_sse_delta_line(""), None);
    }

    #[test]
    #[allow(clippy::missing_panics_doc)]
    fn test_llm_client_compiles() {
        // This test just verifies the client compiles — no live API calls.
        let config = LlmConfig::new("http://localhost:8000/v1", "test-key", "test-model");
        let _client = LlmClient::new(config);
    }

    #[test]
    fn test_build_endpoint_no_trailing_slash() {
        let endpoint = LlmClient::build_endpoint("https://api.openai.com/v1");
        assert_eq!(endpoint, "https://api.openai.com/v1/chat/completions");
    }

    // ── 1A: tool schema + request serialization ─────────────────────────────

    #[test]
    fn test_tool_definition_serializes_to_openai_envelope() {
        let def = ToolDefinition {
            name: "fs_read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        };
        let envelope = ToolEnvelope::from(&def);
        let json = serde_json::to_value(&envelope).expect("serialize");
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "fs_read");
        assert_eq!(json["function"]["description"], "Read a file");
        // input_schema becomes function.parameters (OpenAI shape).
        assert_eq!(json["function"]["parameters"]["required"][0], "path");
    }

    #[test]
    fn test_request_without_tools_omits_tool_fields() {
        // The byte-identical invariant: a tool-less request must NOT emit
        // `tools` or `tool_choice` keys.
        let messages = vec![LlmMessage::user("hi")];
        let req = ChatCompletionsRequest {
            model: "m",
            messages: &messages,
            max_tokens: 100,
            tools: Vec::new(),
            tool_choice: None,
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert!(
            json.get("tools").is_none(),
            "tools must be omitted when empty"
        );
        assert!(
            json.get("tool_choice").is_none(),
            "tool_choice must be omitted when None"
        );
        assert_eq!(json["model"], "m");
        assert_eq!(json["max_tokens"], 100);
    }

    #[test]
    fn test_request_with_tools_includes_them() {
        let messages = vec![LlmMessage::user("hi")];
        let def = ToolDefinition {
            name: "echo".into(),
            description: "Echo".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let req = ChatCompletionsRequest {
            model: "m",
            messages: &messages,
            max_tokens: 100,
            tools: vec![ToolEnvelope::from(&def)],
            tool_choice: Some("auto"),
        };
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(json["tools"][0]["function"]["name"], "echo");
        assert_eq!(json["tool_choice"], "auto");
    }

    #[test]
    fn test_tool_definition_roundtrip() {
        let def = ToolDefinition {
            name: "t".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_string(&def).expect("serialize");
        let back: ToolDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(def, back);
    }

    #[test]
    fn test_tool_less_request_exact_string_serialization() {
        // GPT-5.5 review: prove the "byte-identical" invariant at the STRING
        // level, not just value level — the tool-less request must serialize to
        // exactly the pre-tools shape with no tools/tool_choice keys.
        let messages = vec![LlmMessage::user("hi")];
        let req = ChatCompletionsRequest {
            model: "m",
            messages: &messages,
            max_tokens: 100,
            tools: Vec::new(),
            tool_choice: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert_eq!(
            json,
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":100}"#
        );
    }

    #[test]
    fn test_tool_definition_validate_rejects_bad_name() {
        let bad_chars = ToolDefinition {
            name: "fs read!".into(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        assert!(matches!(
            bad_chars.validate(),
            Err(LlmError::InvalidTool(_))
        ));

        let empty = ToolDefinition {
            name: String::new(),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        assert!(matches!(empty.validate(), Err(LlmError::InvalidTool(_))));

        let too_long = ToolDefinition {
            name: "a".repeat(65),
            description: "d".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        assert!(matches!(too_long.validate(), Err(LlmError::InvalidTool(_))));
    }

    #[test]
    fn test_tool_definition_validate_rejects_non_object_schema() {
        let scalar = ToolDefinition {
            name: "echo".into(),
            description: "d".into(),
            input_schema: serde_json::json!("not an object"),
        };
        assert!(matches!(scalar.validate(), Err(LlmError::InvalidTool(_))));

        let array = ToolDefinition {
            name: "echo".into(),
            description: "d".into(),
            input_schema: serde_json::json!([1, 2, 3]),
        };
        assert!(matches!(array.validate(), Err(LlmError::InvalidTool(_))));
    }

    #[test]
    fn test_tool_definition_validate_accepts_good() {
        let good = ToolDefinition {
            name: "fs_read-v1".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn test_invalid_tool_is_not_retryable() {
        // A malformed tool definition is deterministic — must NOT trigger
        // failover (would just fail identically on every model).
        assert!(!LlmError::InvalidTool("bad".into()).is_retryable());
    }

    #[test]
    fn test_build_endpoint_with_trailing_slash() {
        let endpoint = LlmClient::build_endpoint("https://api.openai.com/v1/");
        assert_eq!(endpoint, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn test_build_endpoint_no_whitespace() {
        let endpoint = LlmClient::build_endpoint("https://api.openai.com/v1");
        assert!(
            !endpoint.contains(' '),
            "endpoint must not contain whitespace"
        );
        assert_eq!(endpoint, "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn test_build_endpoint_custom_base() {
        let endpoint = LlmClient::build_endpoint("http://localhost:8080/v1");
        assert_eq!(endpoint, "http://localhost:8080/v1/chat/completions");
    }

    #[test]
    fn test_build_endpoint_custom_base_with_slash() {
        let endpoint = LlmClient::build_endpoint("http://localhost:8080/v1/");
        assert_eq!(endpoint, "http://localhost:8080/v1/chat/completions");
    }

    // ---- F1 timeout + retryable classification ------------------------------

    #[test]
    fn config_defaults_timeouts_when_unset() {
        // Defaults: no null timeout -> request 60s, connect 10s.
        let cfg = LlmConfig::new("http://x/v1", "k", "m");
        assert_eq!(cfg.request_timeout_ms, None);
        assert_eq!(cfg.connect_timeout_ms, None);
        assert_eq!(
            cfg.request_timeout(),
            Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS)
        );
        assert_eq!(
            cfg.connect_timeout(),
            Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS)
        );
    }

    #[test]
    fn config_timeouts_are_configurable_per_llmconfig() {
        // Layer B can tune this per provider.
        let cfg = LlmConfig::new("http://x/v1", "k", "m")
            .with_request_timeout_ms(2_500)
            .with_connect_timeout_ms(500);
        assert_eq!(cfg.request_timeout(), Duration::from_millis(2_500));
        assert_eq!(cfg.connect_timeout(), Duration::from_millis(500));
    }

    #[test]
    fn config_timeout_serde_roundtrip_and_backward_compat() {
        // The new fields serialize; old JSON without them still deserializes
        // (serde default -> None -> the default applies). Backward-compatible.
        let cfg = LlmConfig::new("http://x/v1", "k", "m").with_request_timeout_ms(1234);
        let json = serde_json::to_string(&cfg).expect("ser");
        assert!(json.contains("request_timeout_ms"));
        let back: LlmConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.request_timeout_ms, Some(1234));

        // Old JSON without timeout fields -> None (does not crash).
        let legacy = r#"{"api_base":"http://x/v1","api_key":"k","model":"m","max_tokens":2048}"#;
        let legacy_cfg: LlmConfig = serde_json::from_str(legacy).expect("legacy de");
        assert_eq!(legacy_cfg.request_timeout_ms, None);
        assert_eq!(legacy_cfg.connect_timeout_ms, None);
    }

    #[test]
    fn timeout_error_is_retryable() {
        // F1 core: a timeout (stuck primary) is retryable -> failover triggers.
        assert!(LlmError::Timeout("slow primary".into()).is_retryable());
    }

    #[test]
    fn http_and_nocontent_errors_are_retryable() {
        // A connection error (dead primary) and empty content -> try the fallback.
        assert!(LlmError::Http("connection refused".into()).is_retryable());
        assert!(LlmError::NoContent.is_retryable());
    }

    #[test]
    fn parse_error_is_not_retryable() {
        // A deterministic parse error gains nothing from grinding the chain.
        assert!(!LlmError::Parse("bad json".into()).is_retryable());
    }

    #[test]
    fn timeout_error_displays_distinctly() {
        let e = LlmError::Timeout("request failed: operation timed out".into());
        let s = format!("{e}");
        assert!(s.starts_with("Timeout error:"), "got: {s}");
    }

    // ── Failover gap #1, step 1 — error taxonomy ────────────────────────────

    #[test]
    fn new_variants_are_retryable_today() {
        // The taxonomy separates the variants, but today all three are still
        // retryable so the chain proceeds as before (a cooldown layer comes later).
        assert!(LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(3),
        }
        .is_retryable());
        assert!(LlmError::AuthFailed("bad key".into()).is_retryable());
        assert!(LlmError::Overloaded("503".into()).is_retryable());
    }

    #[test]
    fn from_status_maps_429_to_rate_limited_with_retry_after() {
        let e = LlmError::from_status(429, "slow down", Some(12));
        match e {
            LlmError::RateLimited {
                message,
                retry_after,
            } => {
                assert_eq!(retry_after, Some(12));
                assert!(message.contains("429"), "got: {message}");
                assert!(message.contains("slow down"), "got: {message}");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn from_status_maps_429_without_retry_after() {
        let e = LlmError::from_status(429, "limited", None);
        assert!(matches!(
            e,
            LlmError::RateLimited {
                retry_after: None,
                ..
            }
        ));
    }

    #[test]
    fn from_status_maps_auth_codes() {
        assert!(matches!(
            LlmError::from_status(401, "[redacted]", None),
            LlmError::AuthFailed(_)
        ));
        assert!(matches!(
            LlmError::from_status(403, "[redacted]", None),
            LlmError::AuthFailed(_)
        ));
    }

    #[test]
    fn from_status_maps_overload_codes() {
        assert!(matches!(
            LlmError::from_status(503, "busy", None),
            LlmError::Overloaded(_)
        ));
        assert!(matches!(
            LlmError::from_status(529, "busy", None),
            LlmError::Overloaded(_)
        ));
    }

    #[test]
    fn from_status_falls_back_to_http_for_other_codes() {
        // 400 (generic body, not "model not found")/418/500 etc. do not
        // belong to a specific class -> generic Http. 404 is now its own
        // class (NotFound) — see from_status_maps_404_to_not_found.
        for code in [400_u16, 418, 500, 502] {
            assert!(
                matches!(LlmError::from_status(code, "x", None), LlmError::Http(_)),
                "status {code} should map to Http"
            );
        }
    }

    #[test]
    fn from_status_maps_404_to_not_found() {
        let e = LlmError::from_status(404, "[redacted]", None);
        assert!(matches!(e, LlmError::NotFound(_)));
        assert!(
            e.is_retryable(),
            "NotFound must stay retryable at chain level"
        );
        assert_eq!(e.failure_class(), LlmFailureClass::ProviderNotFound);
        assert_eq!(e.redacted_status_line(), "HTTP 404");
    }

    #[test]
    fn from_status_maps_400_model_not_found_body_to_not_found() {
        // Production incident shape: NVIDIA NIM returns 400 with a body
        // saying the function/model id is not found (not a real 404).
        let e = LlmError::from_status(
            400,
            "Function id 'acct-abc123/retired-model' not found",
            None,
        );
        assert!(matches!(e, LlmError::NotFound(_)));
        assert_eq!(e.redacted_status_line(), "HTTP 400");
    }

    #[test]
    fn from_status_400_without_not_found_wording_stays_generic_http() {
        // An ordinary validation 400 (bad JSON, bad parameter) must NOT be
        // misclassified as provider-dead.
        let e = LlmError::from_status(400, "invalid request: max_tokens must be positive", None);
        assert!(matches!(e, LlmError::Http(_)));
    }

    #[test]
    fn redacted_status_line_never_contains_raw_body() {
        // The whole point: even though the raw detail carries an
        // account-shaped string, the redacted line only ever exposes the
        // status code + class word.
        let e = LlmError::from_status(404, "acct_9f8e7d6c5b4a leaked account id in body", None);
        let line = e.redacted_status_line();
        assert_eq!(line, "HTTP 404");
        assert!(!line.contains("acct_9f8e7d6c5b4a"));
    }

    #[test]
    fn redacted_status_line_falls_back_to_class_word_without_status() {
        assert_eq!(
            LlmError::Timeout("slow".into()).redacted_status_line(),
            "timeout"
        );
        assert_eq!(LlmError::NoContent.redacted_status_line(), "no_content");
    }

    #[test]
    fn parse_retry_after_handles_integer_seconds() {
        assert_eq!(LlmError::parse_retry_after(Some("30")), Some(30));
        assert_eq!(LlmError::parse_retry_after(Some("  7 ")), Some(7));
    }

    #[test]
    fn parse_retry_after_rejects_non_integer_and_missing() {
        // The HTTP-date format is not supported -> None (the default cooldown takes effect).
        assert_eq!(
            LlmError::parse_retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(LlmError::parse_retry_after(Some("")), None);
        assert_eq!(LlmError::parse_retry_after(None), None);
    }

    #[test]
    fn cooldown_hint_uses_retry_after_when_present() {
        let e = LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(15),
        };
        assert_eq!(e.cooldown_hint(), Some(Duration::from_secs(15)));
    }

    #[test]
    fn cooldown_hint_defaults_for_rate_limit_without_retry_after() {
        let e = LlmError::RateLimited {
            message: "429".into(),
            retry_after: None,
        };
        assert_eq!(
            e.cooldown_hint(),
            Some(LlmError::DEFAULT_RATE_LIMIT_COOLDOWN)
        );
    }

    #[test]
    fn cooldown_hint_for_overloaded_uses_default() {
        assert_eq!(
            LlmError::Overloaded("503".into()).cooldown_hint(),
            Some(LlmError::DEFAULT_OVERLOAD_COOLDOWN)
        );
    }

    #[test]
    fn cooldown_hint_is_none_for_non_rate_errors() {
        assert!(LlmError::AuthFailed("bad".into()).cooldown_hint().is_none());
        assert!(LlmError::Http("conn".into()).cooldown_hint().is_none());
        assert!(LlmError::Timeout("slow".into()).cooldown_hint().is_none());
        assert!(LlmError::NoContent.cooldown_hint().is_none());
        assert!(LlmError::Parse("bad".into()).cooldown_hint().is_none());
        assert!(LlmError::InvalidTool("bad".into())
            .cooldown_hint()
            .is_none());
    }

    #[test]
    fn new_variants_display_distinctly() {
        let rl = LlmError::RateLimited {
            message: "429: limited".into(),
            retry_after: Some(9),
        };
        let s = format!("{rl}");
        assert!(s.contains("Rate limited"), "got: {s}");
        assert!(s.contains("retry after 9s"), "got: {s}");

        assert!(
            format!("{}", LlmError::AuthFailed("HTTP 401: [redacted]".into()))
                .starts_with("Auth failed:")
        );
        assert!(format!("{}", LlmError::Overloaded("HTTP 503: x".into()))
            .starts_with("Provider overloaded:"));
    }

    #[tokio::test]
    async fn client_request_times_out_against_a_hanging_endpoint() {
        // Slow-loris: the endpoint accepts the TCP connection but NEVER responds.
        // With a short request timeout, complete() returns a retryable Timeout
        // error (it does not hang forever). This is F1's unit-test level:
        // "the connection is accepted but we sleep past the timeout".
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // Accept connections but never respond (keep the sockets open).
        tokio::spawn(async move {
            let mut held = Vec::new();
            // Accept until the listener closes; sockets are kept open in `held`
            // (no response) -> the client hangs until its own timeout triggers.
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let cfg = LlmConfig::new(format!("http://{addr}/v1"), "k", "m")
            .with_request_timeout_ms(300)
            .with_connect_timeout_ms(300);
        let client = LlmClient::new(cfg);

        let result = client.complete(&[LlmMessage::user("hei")]).await;
        let err = result.expect_err("hanging endpoint must time out, not hang");
        assert!(
            matches!(err, LlmError::Timeout(_)),
            "expected retryable Timeout, got {err:?}"
        );
        assert!(err.is_retryable(), "timeout must be retryable for failover");
    }

    // ── tool-call response decoding (OpenAI wire shape) ─────────────────────

    #[test]
    fn deserializes_response_with_tool_calls_and_null_content() {
        // Realistic chat-completions response: the assistant chose to call a
        // tool, so `content` is null and `tool_calls` is populated in the
        // OpenAI wire shape (`type` + nested `function` with STRING arguments).
        // Regression for: derived ToolCall::Deserialize expected flat
        // {id,name,arguments} and failed with "error decoding response body".
        let body = r#"{
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "web_fetch",
                            "arguments": "{\"url\":\"https://example.com\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;

        let resp: ChatCompletionsResponse =
            serde_json::from_str(body).expect("tool_calls + null content must decode");

        let choice = resp.choices.into_iter().next().expect("one choice");
        assert!(
            choice.message.content.is_none(),
            "content should decode as None"
        );
        let calls = choice.message.tool_calls.expect("tool_calls present");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc123");
        assert_eq!(calls[0].name, "web_fetch");
        // arguments (a JSON STRING on the wire) parses into a Value object.
        assert_eq!(calls[0].arguments["url"], "https://example.com");
    }

    #[test]
    fn deserializes_tool_call_with_object_arguments_and_missing_type() {
        // Robustness: some OpenAI-compatible providers omit `type` and inline
        // `arguments` as a raw object instead of a JSON string. Both must decode.
        let json = r#"{
            "id": "c1",
            "function": { "name": "fs_read", "arguments": {"path": "a.txt"} }
        }"#;
        let call: ToolCall = serde_json::from_str(json).expect("non-standard shape must decode");
        assert_eq!(call.id, "c1");
        assert_eq!(call.name, "fs_read");
        assert_eq!(call.arguments["path"], "a.txt");
    }

    #[test]
    fn deserializes_tool_call_with_empty_string_arguments() {
        // A no-arg call: providers send arguments as "" — must decode to {}.
        let json = r#"{
            "id": "c2",
            "type": "function",
            "function": { "name": "ping", "arguments": "" }
        }"#;
        let call: ToolCall = serde_json::from_str(json).expect("empty args must decode");
        assert_eq!(call.arguments, serde_json::json!({}));
    }

    #[test]
    fn tool_call_serializes_to_openai_wire_shape() {
        // When an assistant message with tool calls is replayed in a request,
        // each ToolCall must serialize back to the OpenAI shape: top-level
        // `type:"function"` and `function.arguments` as a JSON STRING.
        let call = ToolCall {
            id: "call_xyz".into(),
            name: "search".into(),
            arguments: serde_json::json!({"q": "rust serde"}),
        };
        let v = serde_json::to_value(&call).expect("serialize");
        assert_eq!(v["id"], "call_xyz");
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "search");
        // arguments must be a STRING on the wire, not an object.
        let args = v["function"]["arguments"]
            .as_str()
            .expect("arguments must serialize as a JSON string");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(args).unwrap()["q"],
            "rust serde"
        );
    }

    #[test]
    fn tool_call_wire_roundtrips_through_serde() {
        // Serialize → deserialize preserves the in-memory shape (id/name/args).
        let call = ToolCall {
            id: "c3".into(),
            name: "calc".into(),
            arguments: serde_json::json!({"a": 1, "b": 2.5}),
        };
        let json = serde_json::to_string(&call).expect("ser");
        let back: ToolCall = serde_json::from_str(&json).expect("de");
        assert_eq!(call, back);
    }

    #[tokio::test]
    async fn empty_choices_is_retryable_nocontent_not_parse() {
        // FIX-1 regression: a 200 OK whose `choices` is empty means the model
        // produced nothing. That MUST classify as retryable NoContent (so failover
        // tries the next model) — NOT terminal Parse (which would kill the chain).
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await; // consume request
                let body = r#"{"choices":[]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });

        let cfg = LlmConfig::new(format!("http://{addr}/v1"), "k", "m")
            .with_request_timeout_ms(2000)
            .with_connect_timeout_ms(2000);
        let client = LlmClient::new(cfg);

        let err = client
            .complete(&[LlmMessage::user("hei")])
            .await
            .expect_err("empty choices must be an error");
        assert!(
            matches!(err, LlmError::NoContent),
            "empty choices must be retryable NoContent (not terminal Parse), got {err:?}"
        );
        assert!(
            err.is_retryable(),
            "NoContent must be retryable for failover"
        );
    }

    // ── finish_reason parsing + auto-continuation ───────────────────────────

    // Env vars are process-global; serialize tests that touch them so they
    // don't race each other (same pattern as `watchdog.rs` / `identity.rs`).
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// RAII guard: sets `key` to `value` on construction, restores whatever
    /// was there before on drop (even on panic).
    struct EnvVarGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn deserializes_finish_reason_length_and_stop() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"length"}]}"#;
        let resp: ChatCompletionsResponse = serde_json::from_str(body).expect("decodes");
        let choice = resp.choices.into_iter().next().expect("one choice");
        assert_eq!(
            FinishReason::from_wire(choice.finish_reason.as_deref()),
            FinishReason::Length
        );

        let body2 = r#"{"choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
        let resp2: ChatCompletionsResponse = serde_json::from_str(body2).expect("decodes");
        let choice2 = resp2.choices.into_iter().next().expect("one choice");
        assert_eq!(
            FinishReason::from_wire(choice2.finish_reason.as_deref()),
            FinishReason::Stop
        );
    }

    #[test]
    fn finish_reason_absent_defaults_to_stop_backward_compat() {
        // Backward compat: fixtures/providers that omit finish_reason (like the
        // pre-existing fixtures in this file) must still decode.
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        let resp: ChatCompletionsResponse =
            serde_json::from_str(body).expect("decodes without finish_reason");
        let choice = resp.choices.into_iter().next().expect("one choice");
        assert!(choice.finish_reason.is_none());
        assert_eq!(
            FinishReason::from_wire(choice.finish_reason.as_deref()),
            FinishReason::Stop
        );
    }

    #[test]
    fn finish_reason_tool_calls_and_unknown() {
        assert_eq!(
            FinishReason::from_wire(Some("tool_calls")),
            FinishReason::ToolCalls
        );
        assert_eq!(
            FinishReason::from_wire(Some("content_filter")),
            FinishReason::Other("content_filter".to_string())
        );
        assert!(!FinishReason::Other("content_filter".to_string()).is_length());
        assert!(FinishReason::Length.is_length());
    }

    #[test]
    fn max_continuations_reads_env_override_and_allows_zero() {
        let _lock = env_test_lock();
        {
            let _guard = EnvVarGuard::set("FAMILYCLAW_MAX_CONTINUATIONS", "7");
            assert_eq!(max_continuations(), 7);
        }
        {
            // 0 is a meaningful value ("disabled"), not "unset".
            let _guard = EnvVarGuard::set("FAMILYCLAW_MAX_CONTINUATIONS", "0");
            assert_eq!(max_continuations(), 0);
        }
        std::env::remove_var("FAMILYCLAW_MAX_CONTINUATIONS");
        assert_eq!(max_continuations(), DEFAULT_MAX_CONTINUATIONS);
    }

    /// Minimal blocking HTTP/1.1 mock (no axum dependency) that replies with
    /// the i-th canned body (saturating at the last) for the i-th request
    /// received. Same hand-rolled shape as
    /// `empty_choices_is_retryable_nocontent_not_parse` above and the
    /// `MockLlm` harness in `llm_chain.rs`'s cooldown tests (kept local here
    /// since that one is private to its own module).
    fn spawn_scripted_mock(
        bodies: Vec<String>,
    ) -> (
        std::net::SocketAddr,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
        let addr = listener.local_addr().expect("mock addr");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_t = Arc::clone(&calls);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let n = calls_t.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0_u8; 4096];
                let _ = stream.read(&mut buf);
                let idx = n.min(bodies.len().saturating_sub(1));
                let body = bodies.get(idx).cloned().unwrap_or_default();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });

        (addr, calls)
    }

    // NOTE: these two env-sensitive tests are deliberately plain `#[test]` (not
    // `#[tokio::test]`) driving their own current-thread runtime via
    // `block_on`. That keeps the `_lock`/`_guard` env-serialization guards
    // entirely in synchronous scope (never alive across an `.await` inside an
    // async fn/block), satisfying `clippy::await_holding_lock` — the guards
    // still cover the whole test body since `block_on` blocks synchronously
    // until the future completes.

    #[test]
    fn continuation_concatenates_length_then_stop() {
        let _lock = env_test_lock();
        std::env::remove_var("FAMILYCLAW_MAX_CONTINUATIONS"); // use the default (3)

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        rt.block_on(async {
            let body1 = r#"{"choices":[{"message":{"role":"assistant","content":"partial-one "},"finish_reason":"length"}]}"#.to_string();
            let body2 = r#"{"choices":[{"message":{"role":"assistant","content":"final-part"},"finish_reason":"stop"}]}"#.to_string();
            let (addr, calls) = spawn_scripted_mock(vec![body1, body2]);

            let cfg = LlmConfig::new(format!("http://{addr}/v1"), "k", "m")
                .with_request_timeout_ms(2000)
                .with_connect_timeout_ms(2000);
            let client = LlmClient::new(cfg);

            let text = client
                .complete(&[LlmMessage::user("hi")])
                .await
                .expect("completes");
            assert_eq!(text, "partial-one final-part");
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "exactly one continuation round (length -> stop)"
            );
        });
    }

    #[test]
    fn continuation_disabled_when_max_continuations_zero() {
        let _lock = env_test_lock();
        let _guard = EnvVarGuard::set("FAMILYCLAW_MAX_CONTINUATIONS", "0");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        rt.block_on(async {
            let body1 = r#"{"choices":[{"message":{"role":"assistant","content":"only-part"},"finish_reason":"length"}]}"#.to_string();
            let (addr, calls) = spawn_scripted_mock(vec![body1]);

            let cfg = LlmConfig::new(format!("http://{addr}/v1"), "k", "m")
                .with_request_timeout_ms(2000)
                .with_connect_timeout_ms(2000);
            let client = LlmClient::new(cfg);

            let text = client
                .complete(&[LlmMessage::user("hi")])
                .await
                .expect("completes even without continuation");
            assert_eq!(text, "only-part");
            assert_eq!(
                calls.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "FAMILYCLAW_MAX_CONTINUATIONS=0 must disable continuation entirely"
            );
        });
    }

    #[tokio::test]
    async fn continuation_failure_returns_accumulated_partial_not_error() {
        // Only ONE canned body is provided but the mock server itself stays up
        // (listener still accepting) — the continuation round's connection
        // will be answered with the same (already-"length") body again and
        // again up to max_continuations, OR we simulate a hard failure by
        // pointing the continuation at a closed port. Simplest deterministic
        // approach: bind a listener, accept exactly one connection (the first
        // call), then drop the listener so the second connection is refused.
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind");
        let addr = listener.local_addr().expect("mock addr");

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0_u8; 4096];
                let _ = stream.read(&mut buf);
                let body = r#"{"choices":[{"message":{"role":"assistant","content":"partial-only"},"finish_reason":"length"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
            // Listener is dropped here — the port closes, so the
            // continuation's connection attempt fails (connection refused).
        });

        let cfg = LlmConfig::new(format!("http://{addr}/v1"), "k", "m")
            .with_request_timeout_ms(2000)
            .with_connect_timeout_ms(2000);
        let client = LlmClient::new(cfg);

        let text = client
            .complete(&[LlmMessage::user("hi")])
            .await
            .expect("must return the accumulated partial, not an error");
        assert_eq!(
            text, "partial-only",
            "a failed continuation call must not lose the already-accumulated partial text"
        );
    }

    // ── Gemini wire format (ANY-AI provider support) ────────────────────────

    #[test]
    fn gemini_wire_format_is_default_openai_chat() {
        // Backward compatibility: every LlmConfig built before this field
        // existed must behave identically — default is OpenAiChat.
        let cfg = LlmConfig::new("https://api.openai.com/v1", "k", "m");
        assert_eq!(cfg.wire_format, LlmWireFormat::OpenAiChat);
    }

    #[test]
    fn gemini_wire_format_serde_defaults_on_missing_field() {
        // A config serialized before `wire_format` existed (no field in the
        // JSON at all) must still deserialize -> OpenAiChat, not fail.
        let json = r#"{"api_base":"https://api.openai.com/v1","api_key":"k","model":"gpt-4o","max_tokens":4096}"#;
        let cfg: LlmConfig = serde_json::from_str(json).expect("old-shape config must decode");
        assert_eq!(cfg.wire_format, LlmWireFormat::OpenAiChat);
    }

    #[test]
    fn gemini_wire_format_roundtrips_through_serde() {
        let cfg = LlmConfig::new("https://generativelanguage.googleapis.com/v1beta", "k", "gemini-2.5-pro")
            .with_wire_format(LlmWireFormat::GeminiGenerate);
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(json.contains("\"gemini_generate\""), "got: {json}");
        let back: LlmConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.wire_format, LlmWireFormat::GeminiGenerate);
    }

    #[test]
    fn gemini_endpoint_builds_generate_content_path() {
        let url = LlmClient::gemini_endpoint(
            "https://generativelanguage.googleapis.com/v1beta",
            "gemini-2.5-pro",
        );
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
        // Trailing slash on api_base must not produce a double slash.
        let url2 = LlmClient::gemini_endpoint(
            "https://generativelanguage.googleapis.com/v1beta/",
            "gemini-2.5-pro",
        );
        assert_eq!(url2, url);
    }

    #[test]
    fn gemini_request_maps_system_user_assistant_roles() {
        let messages = vec![
            LlmMessage::system("You are a helpful sibling."),
            LlmMessage::user("hei"),
            LlmMessage::assistant("hei sisko"),
            LlmMessage::user("mitä kuuluu?"),
        ];
        let req = GeminiGenerateContentRequest::from_messages(&messages, 2048);

        // System messages are extracted into systemInstruction, not `contents`.
        let sys = req
            .system_instruction
            .as_ref()
            .expect("system instruction present");
        assert_eq!(sys.parts[0].text.as_deref(), Some("You are a helpful sibling."));

        assert_eq!(req.contents.len(), 3, "system message excluded from contents");
        assert_eq!(req.contents[0].role, "user");
        assert_eq!(req.contents[0].parts[0].text.as_deref(), Some("hei"));
        assert_eq!(req.contents[1].role, "model", "assistant -> Gemini's 'model' role");
        assert_eq!(req.contents[1].parts[0].text.as_deref(), Some("hei sisko"));
        assert_eq!(req.contents[2].role, "user");

        assert_eq!(req.generation_config.max_output_tokens, 2048);
    }

    #[test]
    fn gemini_request_omits_system_instruction_when_no_system_messages() {
        let messages = vec![LlmMessage::user("hei")];
        let req = GeminiGenerateContentRequest::from_messages(&messages, 4096);
        assert!(req.system_instruction.is_none());
    }

    #[test]
    fn gemini_request_serializes_camel_case_field_names() {
        let messages = vec![LlmMessage::system("be nice"), LlmMessage::user("hei")];
        let req = GeminiGenerateContentRequest::from_messages(&messages, 1024);
        let v = serde_json::to_value(&req).expect("serialize");
        assert!(v.get("contents").is_some());
        assert!(v.get("systemInstruction").is_some(), "camelCase field, got: {v}");
        assert_eq!(v["generationConfig"]["maxOutputTokens"], 1024);
    }

    #[test]
    fn gemini_response_parses_text_and_stop_finish_reason() {
        let body = r#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hei sisko"}]},
                "finishReason": "STOP"
            }]
        }"#;
        let resp: GeminiGenerateContentResponse =
            serde_json::from_str(body).expect("gemini response must decode");
        let candidate = resp.candidates.into_iter().next().expect("one candidate");
        assert_eq!(
            candidate.content.expect("content").parts[0].text.as_deref(),
            Some("hei sisko")
        );
        assert_eq!(
            FinishReason::from_gemini_wire(candidate.finish_reason.as_deref()),
            FinishReason::Stop
        );
    }

    #[test]
    fn gemini_response_maps_max_tokens_to_length_finish_reason() {
        assert_eq!(
            FinishReason::from_gemini_wire(Some("MAX_TOKENS")),
            FinishReason::Length
        );
        assert!(FinishReason::from_gemini_wire(Some("MAX_TOKENS")).is_length());
        assert_eq!(FinishReason::from_gemini_wire(None), FinishReason::Stop);
        assert_eq!(
            FinishReason::from_gemini_wire(Some("SAFETY")),
            FinishReason::Other("SAFETY".to_string())
        );
    }

    #[test]
    fn gemini_response_multi_part_text_is_concatenated() {
        // Gemini can split one candidate's text across multiple `parts`.
        let body = r#"{
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "hei "}, {"text": "sisko"}]},
                "finishReason": "STOP"
            }]
        }"#;
        let resp: GeminiGenerateContentResponse =
            serde_json::from_str(body).expect("decode");
        let candidate = resp.candidates.into_iter().next().expect("candidate");
        let joined: String = candidate
            .content
            .expect("content")
            .parts
            .into_iter()
            .filter_map(|p| p.text)
            .collect();
        assert_eq!(joined, "hei sisko");
    }

    /// End-to-end: the Gemini wire format actually talks to the network
    /// through [`LlmClient::complete`] (the SAME public entry point the
    /// OpenAI wire format uses), proving the dispatch in
    /// [`LlmClient::complete_once`] is real, not just parsed in isolation.
    /// Reuses the [`spawn_scripted_mock`] harness above (canned HTTP/1.1
    /// responses over a raw TCP listener, no mocking crate dependency).
    #[tokio::test]
    async fn gemini_wire_format_completes_end_to_end_via_mock_server() {
        let body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hei sisko!"}]},"finishReason":"STOP"}]}"#.to_string();
        let (addr, calls) = spawn_scripted_mock(vec![body]);

        let cfg = LlmConfig::new(format!("http://{addr}/v1beta"), "test-key", "gemini-2.5-pro")
            .with_wire_format(LlmWireFormat::GeminiGenerate)
            .with_request_timeout_ms(2000)
            .with_connect_timeout_ms(2000);
        let client = LlmClient::new(cfg);

        let text = client
            .complete(&[
                LlmMessage::system("be nice"),
                LlmMessage::user("hei"),
            ])
            .await
            .expect("gemini wire format must complete via the same public entry point");
        assert_eq!(text, "hei sisko!");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gemini_wire_format_complete_with_tools_empty_delegates_to_text_path() {
        let body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"no tools needed"}]},"finishReason":"STOP"}]}"#.to_string();
        let (addr, _calls) = spawn_scripted_mock(vec![body]);

        let cfg = LlmConfig::new(format!("http://{addr}/v1beta"), "k", "gemini-2.5-pro")
            .with_wire_format(LlmWireFormat::GeminiGenerate)
            .with_request_timeout_ms(2000)
            .with_connect_timeout_ms(2000);
        let client = LlmClient::new(cfg);

        let result = client
            .complete_with_tools(&[LlmMessage::user("hei")], &[])
            .await
            .expect("empty tools list must delegate to the tool-less Gemini path");
        assert_eq!(result.text(), "no tools needed");
        assert!(!result.has_tool_calls());
    }

    #[tokio::test]
    async fn gemini_wire_format_complete_with_tools_nonempty_is_unimplemented() {
        // No network call should even happen — the tools-non-empty case is
        // rejected before building a request (see complete_with_tools_choice).
        let cfg = LlmConfig::new("http://127.0.0.1:1", "k", "gemini-2.5-pro")
            .with_wire_format(LlmWireFormat::GeminiGenerate);
        let client = LlmClient::new(cfg);

        let tool = ToolDefinition {
            name: "ping".into(),
            description: "ping".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        let err = client
            .complete_with_tools(&[LlmMessage::user("hei")], &[tool])
            .await
            .expect_err("Gemini function calling must not be silently attempted");
        assert!(
            matches!(err, LlmError::Http(_)),
            "expected a clear Http error, got {err:?}"
        );
        assert!(
            format!("{err}").contains("gemini_generate"),
            "error should name the unimplemented wire format, got: {err}"
        );
    }

    #[tokio::test]
    async fn gemini_wire_format_streaming_is_unimplemented() {
        let cfg = LlmConfig::new("http://127.0.0.1:1", "k", "gemini-2.5-pro")
            .with_wire_format(LlmWireFormat::GeminiGenerate);
        let client = LlmClient::new(cfg);

        // `LlmChunkStream`'s Ok variant isn't Debug, so match manually
        // instead of `expect_err` (which requires Debug on the Ok type).
        match client.complete_stream(&[LlmMessage::user("hei")]).await {
            Ok(_) => panic!("Gemini streaming must not be silently attempted as OpenAI SSE"),
            Err(err) => assert!(matches!(err, LlmError::Http(_))),
        }
    }

    #[tokio::test]
    async fn anthropic_and_bedrock_wire_formats_error_before_any_network_call() {
        // Both are design-doc-only (not implemented). Point at a
        // guaranteed-unroutable address (port 1, no listener) to prove the
        // error comes from the dispatch guard, not a real connection
        // failure that happened to also produce an Http error.
        for format in [LlmWireFormat::AnthropicMessages, LlmWireFormat::Bedrock] {
            let cfg = LlmConfig::new("http://127.0.0.1:1", "k", "m").with_wire_format(format);
            let client = LlmClient::new(cfg);
            let err = client
                .complete(&[LlmMessage::user("hei")])
                .await
                .expect_err("unimplemented wire format must error, not silently fall back");
            assert!(matches!(err, LlmError::Http(_)));
            let msg = format!("{err}");
            assert!(
                msg.contains("multi-provider-wire-formats.md"),
                "error should point at the design doc, got: {msg}"
            );
        }
    }

    #[test]
    fn llm_wire_format_as_word_is_stable() {
        assert_eq!(LlmWireFormat::OpenAiChat.as_word(), "openai_chat");
        assert_eq!(LlmWireFormat::GeminiGenerate.as_word(), "gemini_generate");
        assert_eq!(LlmWireFormat::AnthropicMessages.as_word(), "anthropic_messages");
        assert_eq!(LlmWireFormat::Bedrock.as_word(), "bedrock");
    }
}

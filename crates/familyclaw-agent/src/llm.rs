//! LLM HTTP client — OpenAI-compatible chat completions API.
//!
//! Generic client that calls any OpenAI-compatible endpoint (e.g., `OpenAI`,
//! local LLM servers). Configuration is loaded at runtime — never hardcoded.
//!
//! **KERROS A only:** No family-specific names, souls, or private data.

use std::pin::Pin;
use std::time::Duration;

use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Oletus **request**-timeout (koko pyyntö, ml. vastauksen luku) jos
/// [`LlmConfig::request_timeout_ms`] ei ole asetettu. 60 s on järkevä yläraja
/// LLM-completionille: tarpeeksi väljä hitaalle mallille, mutta riittävän
/// tiukka ettei jumittunut primary blokkaa failoveria ikuisesti (F1).
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;

/// Oletus **connect**-timeout (TCP/TLS-kättely) jos
/// [`LlmConfig::connect_timeout_ms`] ei ole asetettu. 10 s erottaa "endpoint
/// ei vastaa lainkaan" -tilanteen hitaasta generoinnista — connect-vaihe ei
/// saa nojata koko 60 s request-budjettiin.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;

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
    /// Koko pyynnön (request + vastauksen luku) timeout millisekunteina.
    /// `None` → [`DEFAULT_REQUEST_TIMEOUT_MS`]. KERROS B voi virittää tämän
    /// per provider (esim. nopealle endpointille tiukempi, hitaalle väljempi).
    /// **F1:** ilman timeoutia jumittunut primary blokkaisi
    /// [`crate::llm_chain::LlmFailover::complete`]:n ikuisesti — timeout
    /// pakottaa kuolleen primaryn antautumaan, jolloin failover laukeaa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
    /// TCP/TLS-yhteyden muodostuksen timeout millisekunteina. `None` →
    /// [`DEFAULT_CONNECT_TIMEOUT_MS`]. Erottaa "ei kuuntele / ei reititystä"
    /// -tilanteen hitaasta generoinnista.
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
            max_tokens: 2048, // default
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

    /// Asettaa koko pyynnön timeoutin (millisekunteina). KERROS B -viritys per
    /// provider. Ks. [`DEFAULT_REQUEST_TIMEOUT_MS`].
    #[must_use]
    pub fn with_request_timeout_ms(mut self, ms: u64) -> Self {
        self.request_timeout_ms = Some(ms);
        self
    }

    /// Asettaa yhteyden muodostuksen timeoutin (millisekunteina). KERROS B
    /// -viritys per provider. Ks. [`DEFAULT_CONNECT_TIMEOUT_MS`].
    #[must_use]
    pub fn with_connect_timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = Some(ms);
        self
    }

    /// Efektiivinen request-timeout [`Duration`]:na (oletus täytetty).
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(
            self.request_timeout_ms
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS),
        )
    }

    /// Efektiivinen connect-timeout [`Duration`]:na (oletus täytetty).
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
    /// **F1:** rakentaa `reqwest::Client`:n **request- ja connect-timeoutilla**
    /// ([`LlmConfig::request_timeout`] / [`LlmConfig::connect_timeout`]). Ilman
    /// timeoutia jumittunut primary (yhteys hyväksytään mutta vastaus ei tule)
    /// blokkaisi [`crate::llm_chain::LlmFailover::complete`]:n ikuisesti, eikä
    /// failover laukeaisi koskaan. Timeout muuttaa hyytyneen primaryn
    /// **retryable** [`LlmError::Timeout`]-virheeksi, jolloin ketju siirtyy
    /// fallbackiin.
    ///
    /// Jos `reqwest::Client`:n rakennus epäonnistuu (epätavallista — esim.
    /// TLS-backendin alustus), palataan oletusklienttiin ilman timeoutteja,
    /// jotta konstruktori pysyy infallible-rajapinnaltaan (`#[must_use]`, ei
    /// `Result`). Tämä on turvallinen degradaatio: failover toimii yhä
    /// connection-virheillä, vain hang-suoja menetetään äärimmäistapauksessa.
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

    /// **Failover gap #1, step 1.** Luokittelee ei-onnistuneen HTTP-vastauksen
    /// oikeaan [`LlmError`]-varianttiin. Kutsutaan VAIN kun
    /// `response.status().is_success()` on `false`. Poimii `Retry-After`-headerin
    /// (429:lle), redaktoi bodyn auth-virheissä (401/403) avainvuotojen
    /// estämiseksi, ja delegoi luokittelun puhtaalle [`LlmError::from_status`]:lle
    /// joka on suoraan yksikkötestattavissa ilman verkkoa.
    async fn error_from_response(response: reqwest::Response) -> LlmError {
        let status = response.status().as_u16();
        let retry_after = LlmError::parse_retry_after(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
        );
        // Redact body for auth errors to prevent API key leakage in logs.
        let is_auth = status == 401 || status == 403;
        let detail = if is_auth {
            "[redacted]".to_string()
        } else {
            response.text().await.unwrap_or_default()
        };
        LlmError::from_status(status, &detail, retry_after)
    }

    /// Completes a chat conversation, returning the response text.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    pub async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
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
            // Body-luvun timeout (koko-request-budjetti ylittyi vastausta
            // luettaessa) → retryable Timeout (F1); aito dekoodausvirhe → Parse.
            if e.is_timeout() {
                LlmError::Timeout(format!("response read timed out: {e}"))
            } else {
                LlmError::Parse(format!("response parse error: {e}"))
            }
        })?;

        chat_response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            // Empty choices / null content = the model produced nothing this turn.
            // That is RETRYABLE (another model in the chain may produce content),
            // so classify it as NoContent — NOT Parse (which is terminal and would
            // defeat failover). [F1 invariant: see LlmError::is_retryable]
            .ok_or(LlmError::NoContent)
    }

    /// Avaa SSE-striimauksen (`stream: true`) ja palauttaa tekstipätkien virran.
    ///
    /// # Errors
    /// Palauttaa virheen jos HTTP-pyyntö epäonnistuu ennen striimin avaamista.
    pub async fn complete_stream(
        &self,
        messages: &[LlmMessage],
    ) -> std::result::Result<LlmChunkStream, LlmError> {
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
            // Body-luvun timeout (koko-request-budjetti ylittyi vastausta
            // luettaessa) → retryable Timeout (F1); aito dekoodausvirhe → Parse.
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

/// Striimattujen tekstipätkien virta ([`LlmClient::complete_stream`]).
pub type LlmChunkStream = Pin<Box<dyn Stream<Item = std::result::Result<String, LlmError>> + Send>>;

/// LLM error types.
///
/// **Failover gap #1, step 1 — error taxonomy.** Aiemmin *kaikki* ei-onnistuneet
/// HTTP-statukset romahtivat yhteen [`LlmError::Http`]-luokkaan, jolloin 429
/// (rate limit), 401/403 (auth/billing) ja 503/529 (overloaded) eivät
/// erottuneet toisistaan — eriytetty backoff/rotation oli rakenteellisesti
/// mahdotonta. Nämä variantit *erottavat* failoverille kriittiset tapaukset,
/// jotta tuleva cooldown/key-rotation -kerros voi haaroittua niihin
/// ([`LlmError::cooldown_hint`] tarjoaa siemenen). Tämä askel ei vielä rakenna
/// itse backoff-tilakonetta — vain taksonomian ja sen propagoinnin.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// HTTP request failed (yhteysvirhe tai muu ei-onnistunut statuskoodi joka
    /// ei kuulu tarkempaan luokkaan: yleinen 4xx/5xx, ECONNREFUSED yms.).
    Http(String),
    /// Pyyntö aikakatkaistiin (request- tai connect-timeout). **F1:** tämä on
    /// **retryable** — jumittunut primary antautuu timeoutilla, ja
    /// [`crate::llm_chain::LlmFailover`] siirtyy seuraavaan fallbackiin.
    Timeout(String),
    /// HTTP 429 — provider rate-limited tämän avaimen/mallin. Retryable, mutta
    /// **erotettu** [`LlmError::Http`]:sta tarkoituksella: tuleva backoff-kerros
    /// kohtelee tätä erikseen (odota `retry_after` sekuntia ja/tai kierrätä
    /// avain-pool). `retry_after` poimitaan `Retry-After`-headerista (sekunteja)
    /// jos provider sen antaa. [Failover gap #1, step 1]
    RateLimited {
        /// Provider-viesti / -konteksti (lokeja varten).
        message: String,
        /// `Retry-After`-headerin arvo sekunteina, jos provider antoi sen.
        retry_after: Option<u64>,
    },
    /// HTTP 401/403 — avain on virheellinen, vanhentunut tai laskutus on
    /// loppunut. **Avain-poolin rotaatiosignaali, EI mallin-fallback-signaali:**
    /// sama avain epäonnistuu jokaisella mallilla, joten tuleva kerros vaihtaa
    /// avainta (ei mallia). Body redaktoidaan vuotojen estämiseksi.
    /// [Failover gap #1, step 1]
    AuthFailed(String),
    /// HTTP 503/529 — provider on hetkellisesti ylikuormitettu. Retryable
    /// backoffilla; **erotettu** [`LlmError::Http`]:sta jotta tuleva kerros voi
    /// odottaa eskaloituvalla viiveellä saman providerin sijaan kuin jysäyttää
    /// ketjun läpi. [Failover gap #1, step 1]
    Overloaded(String),
    /// Response parsing failed
    Parse(String),
    /// No content in response
    NoContent,
    /// A tool definition advertised to the model is malformed (bad name or
    /// non-object schema). Deterministic config error → **not** retryable.
    InvalidTool(String),
}

impl LlmError {
    /// Onko virhe **uudelleenyritettävä** (failover saa kokeilla seuraavaa
    /// klienttiä)?
    ///
    /// **F1-ydin:** [`LlmError::Timeout`] (jumittunut/hyytynyt primary) on
    /// retryable, samoin [`LlmError::Http`] (yhteysvirhe tai 5xx/429 -tyyppinen
    /// hetkellinen häiriö) ja [`LlmError::NoContent`] (toinen malli voi tuottaa
    /// sisällön). Vain [`LlmError::Parse`] **ei** ole retryable: sama vastaus
    /// jäsentyisi seuraavallakin yrityksellä samalla mallilla samaan virheeseen,
    /// joten se on deterministinen — mutta koska failover kokeilee *eri*
    /// klienttejä (eri malli/endpoint), parse-virhe yhdellä mallilla ei kerro
    /// mitään seuraavasta. Konservatiivisesti: kohtele parse-virhettä
    /// **ei-retryable**:na, jotta ilmeisen rikkinäinen pyyntö (esim. väärä
    /// request-muoto) ei jauha koko ketjua turhaan; verkko-/timeout-/sisältö-
    /// luokat ovat retryable.
    /// **Failover gap #1, step 1 — taksonomian propagointi:** uudet variantit
    /// [`LlmError::RateLimited`], [`LlmError::AuthFailed`] ja
    /// [`LlmError::Overloaded`] ovat KAIKKI tällä hetkellä retryable, jotta
    /// ketju etenee *tänään* täsmälleen kuten ennen (yksikään näistä ei ole
    /// terminaalinen). Ero on että variantit ovat nyt **erillisiä**, joten
    /// tuleva cooldown/key-rotation -kerros voi haaroittua niihin:
    /// `RateLimited` → odota `retry_after` ([`Self::cooldown_hint`]),
    /// `AuthFailed` → kierrätä avain (ei mallia), `Overloaded` → eskaloituva
    /// backoff. Vain [`LlmError::Parse`] ja [`LlmError::InvalidTool`] ovat
    /// deterministisesti ei-retryable.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            LlmError::Timeout(_)
            | LlmError::Http(_)
            | LlmError::NoContent
            // RateLimited/AuthFailed/Overloaded: distinct variants, mutta tänään
            // yhä retryable jotta ketju etenee (cooldown-kerros tulee myöhemmin).
            | LlmError::RateLimited { .. }
            | LlmError::AuthFailed(_)
            | LlmError::Overloaded(_) => true,
            // Parse + InvalidTool are deterministic: the same request would fail
            // identically, so do not grind the whole chain.
            LlmError::Parse(_) | LlmError::InvalidTool(_) => false,
        }
    }

    /// **Failover gap #1, step 1 — siemen tulevalle backoff-tilakoneelle.**
    /// Palauttaa *ehdotetun* odotusajan ennen kuin sama provider/avain kannattaa
    /// uudelleenyrittää. Tämä on PUHDAS vihje (ei nuku, ei mutatoi tilaa): tuleva
    /// cooldown/rotation -kerros kuluttaa sen — nykyinen [`LlmFailover`] ei vielä
    /// käytä sitä, joten käytös ei muutu.
    ///
    /// - [`LlmError::RateLimited`] → `Retry-After`-arvo jos provider antoi sen
    ///   (sekunteina), muuten oletus [`Self::DEFAULT_RATE_LIMIT_COOLDOWN`].
    /// - [`LlmError::Overloaded`] → oletus [`Self::DEFAULT_OVERLOAD_COOLDOWN`]
    ///   (provider toipuu — odota ennen samalle providerille palaamista).
    /// - Kaikki muut → `None` (ei luonteva cooldown; failover vaihtaa
    ///   klienttiä välittömästi).
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
            | LlmError::Parse(_)
            | LlmError::NoContent
            | LlmError::InvalidTool(_) => None,
        }
    }

    /// Oletus-cooldown 429:lle kun provider EI anna `Retry-After`-headeria.
    /// Maltillinen 5 s — tulevan backoff-kerroksen lähtöarvo, ei lopullinen.
    pub const DEFAULT_RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(5);

    /// Oletus-cooldown 503/529 (overloaded) -tapaukselle. 2 s: provider on
    /// elossa mutta tukkoinen, lyhyempi odotus kuin rate-limitissä riittää.
    pub const DEFAULT_OVERLOAD_COOLDOWN: Duration = Duration::from_secs(2);

    /// **Failover gap #1, step 1 — testattava seam.** Kuvaa ei-onnistuneen
    /// HTTP-statuskoodin oikeaan [`LlmError`]-varianttiin. `detail` on jo
    /// (auth-tapauksessa redaktoitu) body/konteksti-viesti; `retry_after` on
    /// `Retry-After`-headerista parsittu sekuntimäärä jos läsnä.
    ///
    /// - 429 → [`LlmError::RateLimited`] (`retry_after` mukaan).
    /// - 401/403 → [`LlmError::AuthFailed`] (avain-rotaatiosignaali).
    /// - 503/529 → [`LlmError::Overloaded`] (provider ylikuormitettu).
    /// - muut → [`LlmError::Http`].
    #[must_use]
    fn from_status(status: u16, detail: &str, retry_after: Option<u64>) -> Self {
        match status {
            429 => LlmError::RateLimited {
                message: format!("HTTP 429: {detail}"),
                retry_after,
            },
            401 | 403 => LlmError::AuthFailed(format!("HTTP {status}: {detail}")),
            503 | 529 => LlmError::Overloaded(format!("HTTP {status}: {detail}")),
            _ => LlmError::Http(format!("HTTP {status}: {detail}")),
        }
    }

    /// Parsii `Retry-After`-headerin **sekunneiksi**. OpenAI-yhteensopivat
    /// providerit antavat tyypillisesti kokonaisluvun (delta-seconds); HTTP-date
    /// -muotoa EI tueta tässä (palautuu `None`, jolloin oletus-cooldown astuu
    /// voimaan). Puhdas funktio → suoraan testattavissa.
    #[must_use]
    fn parse_retry_after(value: Option<&str>) -> Option<u64> {
        value?.trim().parse::<u64>().ok()
    }

    /// Kuvaa `reqwest::Error`:n oikeaan [`LlmError`]-luokkaan: aito timeout →
    /// [`LlmError::Timeout`] (retryable, F1); kaikki muut (ml. ECONNREFUSED
    /// `is_connect()`) → [`LlmError::Http`], joka on niin ikään retryable.
    /// Näin sekä jumittunut primary (timeout) että kuollut primary
    /// (yhteysvirhe) laukaisevat failoverin, mutta virheluokat erottuvat
    /// lokeissa ja [`is_retryable`](LlmError::is_retryable):n semantiikassa.
    /// `context` etuliittää viestin (esim. `"request failed"`).
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

/// Poimii yhden SSE-`data:`-rivin delta-sisällön. Palauttaa `None` `[DONE]`-riveille.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_config_creation() {
        let config = LlmConfig::new("https://api.openai.com/v1", "sk-test123", "gpt-4o");
        assert_eq!(config.api_base, "https://api.openai.com/v1");
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.max_tokens, 2048);

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

    // ---- F1 timeout + retryable-luokittelu ------------------------------

    #[test]
    fn config_defaults_timeouts_when_unset() {
        // Oletukset: ei null-timeoutia → request 60s, connect 10s.
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
        // KERROS B voi virittää per provider.
        let cfg = LlmConfig::new("http://x/v1", "k", "m")
            .with_request_timeout_ms(2_500)
            .with_connect_timeout_ms(500);
        assert_eq!(cfg.request_timeout(), Duration::from_millis(2_500));
        assert_eq!(cfg.connect_timeout(), Duration::from_millis(500));
    }

    #[test]
    fn config_timeout_serde_roundtrip_and_backward_compat() {
        // Uudet kentät sarjallistuvat; vanha JSON ilman niitä yhä deserialisoituu
        // (serde default → None → oletus voimassa). Taaksepäin-yhteensopiva.
        let cfg = LlmConfig::new("http://x/v1", "k", "m").with_request_timeout_ms(1234);
        let json = serde_json::to_string(&cfg).expect("ser");
        assert!(json.contains("request_timeout_ms"));
        let back: LlmConfig = serde_json::from_str(&json).expect("de");
        assert_eq!(back.request_timeout_ms, Some(1234));

        // Vanha JSON ilman timeout-kenttiä → None (ei kaadu).
        let legacy = r#"{"api_base":"http://x/v1","api_key":"k","model":"m","max_tokens":2048}"#;
        let legacy_cfg: LlmConfig = serde_json::from_str(legacy).expect("legacy de");
        assert_eq!(legacy_cfg.request_timeout_ms, None);
        assert_eq!(legacy_cfg.connect_timeout_ms, None);
    }

    #[test]
    fn timeout_error_is_retryable() {
        // F1-ydin: timeout (jumittunut primary) on retryable → failover laukeaa.
        assert!(LlmError::Timeout("slow primary".into()).is_retryable());
    }

    #[test]
    fn http_and_nocontent_errors_are_retryable() {
        // Yhteysvirhe (kuollut primary) ja tyhjä sisältö → kokeile fallbackia.
        assert!(LlmError::Http("connection refused".into()).is_retryable());
        assert!(LlmError::NoContent.is_retryable());
    }

    #[test]
    fn parse_error_is_not_retryable() {
        // Deterministinen jäsennysvirhe ei hyödy ketjun jauhamisesta.
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
        // Taksonomia erottaa variantit, mutta tänään kaikki kolme ovat yhä
        // retryable jotta ketju etenee kuten ennen (cooldown-kerros tulee myöhemmin).
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
        // 400/404/500 yms. eivät kuulu tarkkaan luokkaan → yleinen Http.
        for code in [400_u16, 404, 418, 500, 502] {
            assert!(
                matches!(LlmError::from_status(code, "x", None), LlmError::Http(_)),
                "status {code} should map to Http"
            );
        }
    }

    #[test]
    fn parse_retry_after_handles_integer_seconds() {
        assert_eq!(LlmError::parse_retry_after(Some("30")), Some(30));
        assert_eq!(LlmError::parse_retry_after(Some("  7 ")), Some(7));
    }

    #[test]
    fn parse_retry_after_rejects_non_integer_and_missing() {
        // HTTP-date-muotoa ei tueta → None (oletus-cooldown astuu voimaan).
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
        // Slow-loris: endpoint hyväksyy TCP-yhteyden mutta EI vastaa koskaan.
        // Lyhyellä request-timeoutilla complete() palauttaa retryable Timeout-
        // virheen (ei jää roikkumaan ikuisesti). Tämä on F1:n yksikkötaso:
        // "yhteys hyväksytään mutta nukutaan yli timeoutin".
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // Hyväksy yhteydet mutta älä koskaan vastaa (pidä socketit auki).
        tokio::spawn(async move {
            let mut held = Vec::new();
            // Hyväksy kunnes listener sulkeutuu; socketit pidetään `held`:ssä
            // auki (ei vastausta) → asiakas hyytyy kunnes oma timeout laukeaa.
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
}

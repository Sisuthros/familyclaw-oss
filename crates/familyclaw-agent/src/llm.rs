//! LLM HTTP client — OpenAI-compatible chat completions API.
//!
//! Generic client that calls any OpenAI-compatible endpoint (e.g., `OpenAI`,
//! local LLM servers). Configuration is loaded at runtime — never hardcoded.
//!
//! **KERROS A only:** No family-specific names, souls, or private data.

use std::time::Duration;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique ID for this tool call
    pub id: String,
    /// Tool name
    pub name: String,
    /// Arguments as JSON
    pub arguments: serde_json::Value,
}

/// LLM client — stateless HTTP caller for OpenAI-compatible APIs.
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
            let status = response.status();
            // Redact body for auth errors to prevent API key leakage in logs.
            let is_auth = status.as_u16() == 401 || status.as_u16() == 403;
            let detail = if is_auth {
                "[redacted]".to_string()
            } else {
                response.text().await.unwrap_or_default()
            };
            return Err(LlmError::Http(format!("HTTP {status}: {detail}")));
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

    /// Completes a chat conversation and returns both text and tool calls.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    pub async fn complete_with_tools(
        &self,
        messages: &[LlmMessage],
    ) -> Result<CompletionResult, LlmError> {
        let endpoint = Self::build_endpoint(&self.config.api_base);

        let request_body = ChatCompletionsRequest {
            model: &self.config.model,
            messages,
            max_tokens: self.config.max_tokens,
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
            let status = response.status();
            // Redact body for auth errors to prevent API key leakage in logs.
            let is_auth = status.as_u16() == 401 || status.as_u16() == 403;
            let detail = if is_auth {
                "[redacted]".to_string()
            } else {
                response.text().await.unwrap_or_default()
            };
            return Err(LlmError::Http(format!("HTTP {status}: {detail}")));
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

/// LLM error types.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// HTTP request failed (yhteysvirhe tai ei-onnistunut statuskoodi)
    Http(String),
    /// Pyyntö aikakatkaistiin (request- tai connect-timeout). **F1:** tämä on
    /// **retryable** — jumittunut primary antautuu timeoutilla, ja
    /// [`crate::llm_chain::LlmFailover`] siirtyy seuraavaan fallbackiin.
    Timeout(String),
    /// Response parsing failed
    Parse(String),
    /// No content in response
    NoContent,
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
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            LlmError::Timeout(_) | LlmError::Http(_) | LlmError::NoContent => true,
            LlmError::Parse(_) => false,
        }
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
            LlmError::Parse(msg) => write!(f, "Parse error: {msg}"),
            LlmError::NoContent => write!(f, "No content in response"),
        }
    }
}

impl std::error::Error for LlmError {}

// Internal request/response structs for the OpenAI API

#[derive(Serialize)]
struct ChatCompletionsRequest<'a, 'b> {
    model: &'a str,
    messages: &'b [LlmMessage],
    max_tokens: u32,
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

//! LLM HTTP client — OpenAI-compatible chat completions API.
//!
//! Generic client that calls any OpenAI-compatible endpoint (e.g., `OpenAI`,
//! local LLM servers). Configuration is loaded at runtime — never hardcoded.
//!
//! **KERROS A only:** No family-specific names, souls, or private data.

use reqwest::Client;
use serde::{Deserialize, Serialize};

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
        }
    }

    /// Sets maximum tokens.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
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
    #[must_use]
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    /// Returns a reference to the config.
    #[must_use]
    pub const fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// Completes a chat conversation, returning the response text.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    pub async fn complete(&self, messages: &[LlmMessage]) -> Result<String, LlmError> {
        let endpoint = format!(
            "{} /chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

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
            .map_err(|e| LlmError::Http(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Http(format!("HTTP {status}: {body}")));
        }

        let chat_response: ChatCompletionsResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Parse(format!("response parse error: {e}")))?;

        chat_response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| LlmError::Parse("no content in response".into()))
    }

    /// Completes a chat conversation and returns both text and tool calls.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails or the response is invalid.
    pub async fn complete_with_tools(
        &self,
        messages: &[LlmMessage],
    ) -> Result<CompletionResult, LlmError> {
        let endpoint = format!(
            "{} /chat/completions",
            self.config.api_base.trim_end_matches('/')
        );

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
            .map_err(|e| LlmError::Http(format!("request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Http(format!("HTTP {status}: {body}")));
        }

        let chat_response: ChatCompletionsResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Parse(format!("response parse error: {e}")))?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::Parse("no choices in response".into()))?;

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
    /// HTTP request failed
    Http(String),
    /// Response parsing failed
    Parse(String),
    /// No content in response
    NoContent,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(msg) => write!(f, "HTTP error: {msg}"),
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
}

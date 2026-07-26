//! [`OpenAiTts`] -- an `OpenAI`-compatible `/audio/speech` HTTP adapter.
//!
//! ## Why "`OpenAI`-compatible" and not just "`OpenAI`"
//! The request/response shape here (`POST {api_base}/audio/speech` with a
//! JSON body of `{model, input, voice, response_format, speed}`, response =
//! raw encoded audio bytes) is not `OpenAI`-exclusive: several
//! `OpenAI`-API-compatible gateways (Groq, `DeepInfra`, and others) implement
//! the same endpoint. Because [`OpenAiTts::api_base`] is runtime
//! configuration (like [`familyclaw_channels::TelegramChannel`]'s
//! `api_base`), this one adapter covers all of them -- only the base URL,
//! API key, and model name change.
//!
//! ## Auth
//! A single bearer API key (`Authorization: Bearer <key>`), read at runtime
//! from the environment (`OPENAI_API_KEY` by default) or supplied to the
//! constructor -- no OAuth dance, no exotic multi-step auth flow. Same
//! pattern already used for the LLM client
//! (`familyclaw_agent::llm_chain` -- `"OPENAI_API_KEY"`).
//!
//! ## Layer A rules
//! The API key is never hardcoded: it is read at runtime from the
//! environment or supplied to the constructor. `api_base` is also runtime
//! configuration so tests can point it at a mock server.

use tracing::{debug, warn};

use crate::error::TtsError;
use crate::provider::{TtsAudio, TtsFuture, TtsProvider, TtsRequest};

/// The environment variable the API key is read from by default.
pub const API_KEY_ENV: &str = "OPENAI_API_KEY";

/// `OpenAI`'s public API root. The `/audio/speech` path is appended.
pub const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";

/// `OpenAI`'s default TTS model.
pub const DEFAULT_MODEL: &str = "tts-1";

/// `OpenAI`'s default voice.
pub const DEFAULT_VOICE: &str = "alloy";

/// HTTP client timeout: TTS synthesis is a single request/response with no
/// streaming, but can take a few seconds for longer text.
const HTTP_TIMEOUT_SECS: u64 = 60;

/// An `OpenAI`-compatible `/audio/speech` [`TtsProvider`] adapter.
///
/// All settings (API key, `api_base`, `model`, default voice) are runtime
/// configuration -- no hardcoded values. [`OpenAiTts::provider_id`] returns
/// `"openai"` regardless of `api_base`, since the wire protocol (not the
/// origin) determines the adapter identity; callers who need to
/// distinguish e.g. `"openai"` vs. a Groq-backed instance can wrap this in
/// their own routing key.
pub struct OpenAiTts {
    api_key: String,
    api_base: String,
    model: String,
    default_voice: String,
    client: reqwest::Client,
}

impl OpenAiTts {
    /// Creates a client with an explicit API key and the default `OpenAI`
    /// API base and model.
    ///
    /// # Errors
    /// [`TtsError::InvalidInput`] if the API key is empty or the HTTP
    /// client fails to build.
    pub fn new(api_key: impl Into<String>) -> Result<Self, TtsError> {
        Self::with_api_base(api_key, DEFAULT_API_BASE)
    }

    /// Creates a client, reading the API key from the `OPENAI_API_KEY`
    /// environment variable.
    ///
    /// # Errors
    /// [`TtsError::InvalidInput`] if the environment variable is
    /// missing/empty.
    pub fn from_env() -> Result<Self, TtsError> {
        let api_key = std::env::var(API_KEY_ENV).map_err(|_| {
            TtsError::invalid_input(format!(
                "environment variable {API_KEY_ENV} must be set with the OpenAI API key"
            ))
        })?;
        Self::new(api_key)
    }

    /// Creates a client with a custom API root (e.g. a mock server in
    /// tests, or an `OpenAI`-compatible gateway like Groq/`DeepInfra`).
    ///
    /// # Errors
    /// [`TtsError::InvalidInput`] if the API key or `api_base` is empty, or
    /// if building the HTTP client fails.
    pub fn with_api_base(
        api_key: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Result<Self, TtsError> {
        let api_key = api_key.into();
        let api_base = api_base.into();

        if api_key.trim().is_empty() {
            return Err(TtsError::invalid_input("OpenAI API key must not be empty"));
        }
        if api_base.trim().is_empty() {
            return Err(TtsError::invalid_input("api_base must not be empty"));
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| TtsError::invalid_input(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            api_key,
            api_base,
            model: DEFAULT_MODEL.to_string(),
            default_voice: DEFAULT_VOICE.to_string(),
            client,
        })
    }

    /// Overrides the TTS model (default: [`DEFAULT_MODEL`]).
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Overrides the default voice used when a [`TtsRequest`] doesn't
    /// specify one (default: [`DEFAULT_VOICE`]).
    #[must_use]
    pub fn with_default_voice(mut self, voice: impl Into<String>) -> Self {
        self.default_voice = voice.into();
        self
    }
}

/// The JSON body sent to `POST {api_base}/audio/speech`.
#[derive(serde::Serialize)]
struct SpeechRequestBody<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
}

impl TtsProvider for OpenAiTts {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    fn synthesize(&self, request: TtsRequest) -> TtsFuture<'_> {
        Box::pin(async move {
            let url = format!("{}/audio/speech", self.api_base.trim_end_matches('/'));
            let voice = request.voice().unwrap_or(&self.default_voice);
            let body = SpeechRequestBody {
                model: &self.model,
                input: request.text(),
                voice,
                response_format: request.format().as_str(),
                speed: request.speed(),
            };

            debug!(provider = "openai", model = %self.model, voice, "synthesizing speech");

            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| TtsError::request("openai", e.to_string()))?;

            let status = response.status();
            if !status.is_success() {
                let reason = response
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read error body: {e}>"));
                warn!(provider = "openai", %status, "tts backend error");
                return Err(TtsError::backend("openai", status.as_u16(), reason));
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|e| TtsError::request("openai", e.to_string()))?;

            if bytes.is_empty() {
                return Err(TtsError::backend(
                    "openai",
                    status.as_u16(),
                    "provider returned an empty audio payload",
                ));
            }

            Ok(TtsAudio::new(bytes.to_vec(), request.format(), "openai"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_api_key() {
        assert!(matches!(OpenAiTts::new(""), Err(TtsError::InvalidInput(_))));
    }

    #[test]
    fn with_api_base_rejects_empty_base() {
        assert!(matches!(
            OpenAiTts::with_api_base("key", ""),
            Err(TtsError::InvalidInput(_))
        ));
    }

    #[test]
    fn from_env_reports_missing_variable() {
        // SAFETY: test-only removal of a var that (if set at all in this
        // process) is not relied on by other tests running concurrently in
        // this crate -- each test crate binary is a separate process, and
        // no other test in this crate reads OPENAI_API_KEY.
        std::env::remove_var(API_KEY_ENV);
        assert!(matches!(
            OpenAiTts::from_env(),
            Err(TtsError::InvalidInput(_))
        ));
    }

    #[test]
    fn builders_set_model_and_voice() {
        let client = OpenAiTts::new("key")
            .expect("valid")
            .with_model("tts-1-hd")
            .with_default_voice("nova");
        assert_eq!(client.model, "tts-1-hd");
        assert_eq!(client.default_voice, "nova");
    }

    #[test]
    fn provider_id_is_stable() {
        let client = OpenAiTts::new("key").expect("valid");
        assert_eq!(client.provider_id(), "openai");
    }
}

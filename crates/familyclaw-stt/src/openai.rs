//! [`OpenAiWhisper`] -- an `OpenAI`-compatible `/audio/transcriptions`
//! (Whisper) HTTP adapter.
//!
//! ## Why "`OpenAI`-compatible" and not just "`OpenAI`"
//! The request/response shape here (`POST {api_base}/audio/transcriptions`,
//! a multipart upload of `file` + `model` + optional `language`/`prompt` +
//! `response_format`, response = JSON with a `text` field) is not
//! `OpenAI`-exclusive: several `OpenAI`-API-compatible gateways (Groq's
//! Whisper endpoint, `DeepInfra`, and others) implement the same endpoint.
//! Because [`OpenAiWhisper::api_base`] is runtime configuration (like
//! [`familyclaw_tts::OpenAiTts`](../familyclaw_tts/struct.OpenAiTts.html)'s
//! `api_base`), this one adapter covers all of them -- only the base URL,
//! API key, and model name change.
//!
//! ## Auth
//! A single bearer API key (`Authorization: Bearer <key>`), read at runtime
//! from the environment (`OPENAI_API_KEY` by default) or supplied to the
//! constructor -- no OAuth dance, no exotic multi-step auth flow. Same
//! pattern already used for the LLM client
//! (`familyclaw_agent::llm_chain` -- `"OPENAI_API_KEY"`) and
//! `familyclaw_tts::OpenAiTts`.
//!
//! ## Response format
//! Requests always use `response_format=verbose_json`: the plain `json`
//! format only returns `{"text": ...}`, while `verbose_json` additionally
//! reports the detected `language`, which this adapter surfaces on
//! [`familyclaw_stt::SttTranscript::language`](crate::SttTranscript). Both
//! shapes are accepted when parsing the response (`language` is optional),
//! so a gateway that ignores `response_format` and always returns the
//! minimal shape still works.
//!
//! ## Layer A rules
//! The API key is never hardcoded: it is read at runtime from the
//! environment or supplied to the constructor. `api_base` is also runtime
//! configuration so tests can point it at a mock server.

use tracing::{debug, warn};

use crate::error::SttError;
use crate::provider::{SttFuture, SttProvider, SttRequest, SttTranscript};

/// The environment variable the API key is read from by default.
pub const API_KEY_ENV: &str = "OPENAI_API_KEY";

/// `OpenAI`'s public API root. The `/audio/transcriptions` path is
/// appended.
pub const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";

/// `OpenAI`'s default speech-to-text model.
pub const DEFAULT_MODEL: &str = "whisper-1";

/// HTTP client timeout: transcription can take tens of seconds for longer
/// audio clips, so this is generous compared to the TTS adapter's 60s.
const HTTP_TIMEOUT_SECS: u64 = 120;

/// An `OpenAI`-compatible `/audio/transcriptions` [`SttProvider`] adapter.
///
/// All settings (API key, `api_base`, `model`) are runtime configuration --
/// no hardcoded values. [`OpenAiWhisper::provider_id`] returns `"openai"`
/// regardless of `api_base`, since the wire protocol (not the origin)
/// determines the adapter identity; callers who need to distinguish e.g.
/// `"openai"` vs. a Groq-backed instance can wrap this in their own routing
/// key.
pub struct OpenAiWhisper {
    api_key: String,
    api_base: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiWhisper {
    /// Creates a client with an explicit API key and the default `OpenAI`
    /// API base and model.
    ///
    /// # Errors
    /// [`SttError::InvalidInput`] if the API key is empty or the HTTP
    /// client fails to build.
    pub fn new(api_key: impl Into<String>) -> Result<Self, SttError> {
        Self::with_api_base(api_key, DEFAULT_API_BASE)
    }

    /// Creates a client, reading the API key from the `OPENAI_API_KEY`
    /// environment variable.
    ///
    /// # Errors
    /// [`SttError::InvalidInput`] if the environment variable is
    /// missing/empty.
    pub fn from_env() -> Result<Self, SttError> {
        let api_key = std::env::var(API_KEY_ENV).map_err(|_| {
            SttError::invalid_input(format!(
                "environment variable {API_KEY_ENV} must be set with the OpenAI API key"
            ))
        })?;
        Self::new(api_key)
    }

    /// Creates a client with a custom API root (e.g. a mock server in
    /// tests, or an `OpenAI`-compatible gateway like Groq/`DeepInfra`).
    ///
    /// # Errors
    /// [`SttError::InvalidInput`] if the API key or `api_base` is empty, or
    /// if building the HTTP client fails.
    pub fn with_api_base(
        api_key: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Result<Self, SttError> {
        let api_key = api_key.into();
        let api_base = api_base.into();

        if api_key.trim().is_empty() {
            return Err(SttError::invalid_input("OpenAI API key must not be empty"));
        }
        if api_base.trim().is_empty() {
            return Err(SttError::invalid_input("api_base must not be empty"));
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| SttError::invalid_input(format!("failed to build HTTP client: {e}")))?;

        Ok(Self {
            api_key,
            api_base,
            model: DEFAULT_MODEL.to_string(),
            client,
        })
    }

    /// Overrides the transcription model (default: [`DEFAULT_MODEL`]).
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

/// The JSON body `OpenAI` returns for `response_format=verbose_json`
/// (a superset of the plain `json` shape, which only has `text`).
#[derive(serde::Deserialize)]
struct TranscriptionResponseBody {
    text: String,
    #[serde(default)]
    language: Option<String>,
}

impl SttProvider for OpenAiWhisper {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    fn transcribe(&self, request: SttRequest) -> SttFuture<'_> {
        Box::pin(async move {
            let url = format!(
                "{}/audio/transcriptions",
                self.api_base.trim_end_matches('/')
            );

            let filename = format!("audio.{}", request.format().as_str());
            let part = reqwest::multipart::Part::bytes(request.audio().to_vec())
                .file_name(filename)
                .mime_str(request.format().content_type())
                .map_err(|e| SttError::invalid_input(format!("invalid audio content type: {e}")))?;

            let mut form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("model", self.model.clone())
                .text("response_format", "verbose_json");

            if let Some(language) = request.language() {
                form = form.text("language", language.to_string());
            }
            if let Some(prompt) = request.prompt() {
                form = form.text("prompt", prompt.to_string());
            }

            debug!(provider = "openai", model = %self.model, "transcribing audio");

            let response = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .multipart(form)
                .send()
                .await
                .map_err(|e| SttError::request("openai", e.to_string()))?;

            let status = response.status();
            if !status.is_success() {
                let reason = response
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<failed to read error body: {e}>"));
                warn!(provider = "openai", %status, "stt backend error");
                return Err(SttError::backend("openai", status.as_u16(), reason));
            }

            let body: TranscriptionResponseBody = response
                .json()
                .await
                .map_err(|e| SttError::backend("openai", status.as_u16(), e.to_string()))?;

            let mut transcript = SttTranscript::new(body.text, "openai");
            if let Some(language) = body.language {
                transcript = transcript.with_language(language);
            } else if let Some(language) = request.language() {
                transcript = transcript.with_language(language.to_string());
            }
            Ok(transcript)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_api_key() {
        assert!(matches!(
            OpenAiWhisper::new(""),
            Err(SttError::InvalidInput(_))
        ));
    }

    #[test]
    fn with_api_base_rejects_empty_base() {
        assert!(matches!(
            OpenAiWhisper::with_api_base("key", ""),
            Err(SttError::InvalidInput(_))
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
            OpenAiWhisper::from_env(),
            Err(SttError::InvalidInput(_))
        ));
    }

    #[test]
    fn builders_set_model() {
        let client = OpenAiWhisper::new("key")
            .expect("valid")
            .with_model("whisper-large-v3");
        assert_eq!(client.model, "whisper-large-v3");
    }

    #[test]
    fn provider_id_is_stable() {
        let client = OpenAiWhisper::new("key").expect("valid");
        assert_eq!(client.provider_id(), "openai");
    }
}

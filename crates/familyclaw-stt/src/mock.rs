//! [`MockStt`] -- an in-memory [`SttProvider`] with no external SDK, no
//! network, and no API key.
//!
//! Mirrors `familyclaw_tts::MockTts`'s role for the TTS layer (and
//! `familyclaw_channels::MockChannel`'s role for the channel layer): a
//! deterministic stand-in so agent code and tests can exercise the full
//! [`SttProvider`] interface offline. It does not run real speech
//! recognition -- [`MockStt::transcribe`] returns a fixed, configurable
//! transcript (default: `"mock transcript"`), which is enough to assert
//! routing and error-handling behavior without a real STT backend.

use crate::error::SttResult;
use crate::provider::{SttFuture, SttProvider, SttRequest, SttTranscript};

/// The transcript text [`MockStt`] returns by default when none is
/// configured via [`MockStt::with_transcript`].
pub const DEFAULT_TRANSCRIPT: &str = "mock transcript";

/// A no-network, in-memory [`SttProvider`] for tests and offline agent
/// development.
#[derive(Debug, Clone)]
pub struct MockStt {
    provider_id: String,
    transcript: String,
}

impl MockStt {
    /// Builds a mock provider with the default id `"mock"` and the default
    /// transcript ([`DEFAULT_TRANSCRIPT`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider_id: "mock".to_string(),
            transcript: DEFAULT_TRANSCRIPT.to_string(),
        }
    }

    /// Builds a mock provider with a custom id (useful when a test wants to
    /// distinguish several mock instances, e.g. in a routing table).
    #[must_use]
    pub fn with_id(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            transcript: DEFAULT_TRANSCRIPT.to_string(),
        }
    }

    /// Overrides the transcript text returned by [`MockStt::transcribe`],
    /// regardless of the request's audio.
    #[must_use]
    pub fn with_transcript(mut self, transcript: impl Into<String>) -> Self {
        self.transcript = transcript.into();
        self
    }
}

impl Default for MockStt {
    fn default() -> Self {
        Self::new()
    }
}

impl SttProvider for MockStt {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn transcribe(&self, _request: SttRequest) -> SttFuture<'_> {
        let provider_id = self.provider_id.clone();
        let transcript = self.transcript.clone();
        Box::pin(async move {
            let result: SttResult<SttTranscript> = Ok(SttTranscript::new(transcript, provider_id));
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::AudioFormat;

    #[tokio::test]
    async fn transcribe_returns_default_transcript() {
        let mock = MockStt::new();
        let req = SttRequest::new(b"fake-audio".to_vec(), AudioFormat::Wav).expect("valid");
        let transcript = mock.transcribe(req).await.expect("mock never fails");
        assert_eq!(transcript.text, DEFAULT_TRANSCRIPT);
        assert_eq!(transcript.provider, "mock");
    }

    #[test]
    fn provider_id_defaults_and_is_overridable() {
        assert_eq!(MockStt::new().provider_id(), "mock");
        assert_eq!(MockStt::with_id("mock-2").provider_id(), "mock-2");
        assert_eq!(MockStt::default().provider_id(), "mock");
    }

    #[tokio::test]
    async fn transcribe_uses_configured_transcript() {
        let mock = MockStt::new().with_transcript("hei maailma");
        let req = SttRequest::new(b"fake-audio".to_vec(), AudioFormat::Mp3).expect("valid");
        let transcript = mock.transcribe(req).await.expect("mock never fails");
        assert_eq!(transcript.text, "hei maailma");
    }
}

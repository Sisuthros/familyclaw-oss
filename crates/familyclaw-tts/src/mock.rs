//! [`MockTts`] -- an in-memory [`TtsProvider`] with no external SDK, no
//! network, and no API key.
//!
//! Mirrors `familyclaw_channels::MockChannel`'s role for the channel layer:
//! a deterministic stand-in so agent code and tests can exercise the full
//! [`TtsProvider`] interface offline. It does not produce real audio --
//! [`MockTts::synthesize`] returns the UTF-8 bytes of the request text as
//! the "audio" payload, which is enough to assert routing, formatting, and
//! error-handling behavior without a real TTS backend.

use crate::error::TtsResult;
use crate::provider::{TtsAudio, TtsFuture, TtsProvider, TtsRequest};

/// A no-network, in-memory [`TtsProvider`] for tests and offline agent
/// development.
#[derive(Debug, Clone)]
pub struct MockTts {
    provider_id: String,
}

impl MockTts {
    /// Builds a mock provider with the default id `"mock"`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider_id: "mock".to_string(),
        }
    }

    /// Builds a mock provider with a custom id (useful when a test wants to
    /// distinguish several mock instances, e.g. in a routing table).
    #[must_use]
    pub fn with_id(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
        }
    }
}

impl Default for MockTts {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsProvider for MockTts {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn synthesize(&self, request: TtsRequest) -> TtsFuture<'_> {
        let provider_id = self.provider_id.clone();
        Box::pin(async move {
            let bytes = request.text().as_bytes().to_vec();
            let audio: TtsResult<TtsAudio> =
                Ok(TtsAudio::new(bytes, request.format(), provider_id));
            audio
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::AudioFormat;

    #[tokio::test]
    async fn synthesize_echoes_text_as_bytes() {
        let mock = MockTts::new();
        let req = TtsRequest::new("hei maailma").expect("valid");
        let audio = mock.synthesize(req).await.expect("mock never fails");
        assert_eq!(audio.bytes, b"hei maailma");
        assert_eq!(audio.provider, "mock");
        assert_eq!(audio.format, AudioFormat::Mp3);
    }

    #[test]
    fn provider_id_defaults_and_is_overridable() {
        assert_eq!(MockTts::new().provider_id(), "mock");
        assert_eq!(MockTts::with_id("mock-2").provider_id(), "mock-2");
        assert_eq!(MockTts::default().provider_id(), "mock");
    }

    #[tokio::test]
    async fn synthesize_preserves_requested_format() {
        let mock = MockTts::new();
        let req = TtsRequest::new("hei")
            .expect("valid")
            .with_format(AudioFormat::Wav);
        let audio = mock.synthesize(req).await.expect("mock never fails");
        assert_eq!(audio.format, AudioFormat::Wav);
    }
}

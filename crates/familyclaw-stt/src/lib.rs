//! # familyclaw-stt
//!
//! The **speech-to-text (STT) layer** of the `FamilyClaw` platform: a
//! provider-agnostic [`SttProvider`] interface so agents can turn spoken
//! audio into text, plus a real provider adapter.
//!
//! ## What this crate provides
//! - [`SttProvider`] -- the interface for a transcription backend:
//!   [`SttProvider::transcribe`], [`SttProvider::provider_id`].
//!   Dyn-compatible (`Box<dyn SttProvider>`).
//! - [`SttRequest`] -- a validated transcription request (audio bytes +
//!   [`AudioFormat`] + optional language hint/prompt).
//! - [`SttTranscript`] -- the recognized result (text + optional detected
//!   language + provider id).
//! - [`MockStt`] -- an in-memory, no-network provider for tests and offline
//!   agent development (always available, no feature flag).
//! - [`OpenAiWhisper`] (feature `openai`) -- an `OpenAI`-compatible
//!   `/audio/transcriptions` (Whisper) HTTP adapter. Works against `OpenAI`
//!   itself and any `OpenAI`-API-compatible transcription gateway (same
//!   endpoint shape) via a configurable `api_base`. Needs only a bearer API
//!   key -- no exotic auth flow.
//!
//! ## Real adapters are behind feature flags
//! Same rationale as `familyclaw-channels` and `familyclaw-tts`: the
//! default build contains **only** the core abstraction + [`MockStt`], so
//! the platform builds and tests without network access or an API key.
//! Enable `openai` to pull in the real HTTP adapter (`reqwest`, with the
//! `multipart` feature for the file upload).
//!
//! ## More providers: see the design doc
//! This crate ships one verified, working adapter ([`OpenAiWhisper`]).
//! Adding more real backends (a local engine like `whisper.cpp`, Azure
//! Speech, Google Speech-to-Text) is a matter of implementing
//! [`SttProvider`] behind a new feature flag -- see
//! `docs/design/stt-providers.md` for the extension plan and the
//! reserved-flag convention (mirrors `whatsapp`/`signal` in
//! `familyclaw-channels`).
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode API keys or endpoints. Credentials and
//! destinations are runtime configuration; the types carry only the
//! generic structure.
//!
//! ## Example
//! ```
//! # use familyclaw_stt::{AudioFormat, MockStt, SttProvider, SttRequest};
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = MockStt::new().with_transcript("hei maailma");
//! let request = SttRequest::new(b"fake-audio-bytes".to_vec(), AudioFormat::Wav)?;
//! let transcript = provider.transcribe(request).await?;
//! assert_eq!(transcript.text, "hei maailma");
//! assert_eq!(transcript.provider, "mock");
//! # Ok(())
//! # }
//! ```

mod error;
mod mock;
mod provider;

#[cfg(feature = "openai")]
mod openai;

pub use error::{SttError, SttResult};
pub use mock::{MockStt, DEFAULT_TRANSCRIPT};
pub use provider::{AudioFormat, SttFuture, SttProvider, SttRequest, SttTranscript};

#[cfg(feature = "openai")]
pub use openai::{
    OpenAiWhisper, API_KEY_ENV as OPENAI_API_KEY_ENV, DEFAULT_API_BASE as OPENAI_DEFAULT_API_BASE,
    DEFAULT_MODEL as OPENAI_DEFAULT_MODEL,
};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test will fail to compile.
        let req = SttRequest::new(b"hei".to_vec(), AudioFormat::Wav).expect("valid");
        assert_eq!(req.audio(), b"hei");
        let mock = MockStt::new();
        assert_eq!(mock.provider_id(), "mock");
        let err = SttError::invalid_input("x");
        assert!(matches!(err, SttError::InvalidInput(_)));
        let fmt = AudioFormat::Ogg;
        assert_eq!(fmt.as_str(), "ogg");
        assert_eq!(DEFAULT_TRANSCRIPT, "mock transcript");
    }

    #[tokio::test]
    async fn end_to_end_mock_transcription() {
        let provider = MockStt::new();
        let request = SttRequest::new(b"moi".to_vec(), AudioFormat::Mp3).expect("valid");
        let transcript: SttTranscript = provider.transcribe(request).await.expect("mock ok");
        assert_eq!(transcript.text, DEFAULT_TRANSCRIPT);
        assert_eq!(transcript.provider, "mock");
    }
}

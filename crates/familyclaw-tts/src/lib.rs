//! # familyclaw-tts
//!
//! The **text-to-speech (TTS) layer** of the `FamilyClaw` platform: a
//! provider-agnostic [`TtsProvider`] interface so agents can turn text into
//! speech, plus a real provider adapter.
//!
//! ## What this crate provides
//! - [`TtsProvider`] -- the interface for a speech synthesis backend:
//!   [`TtsProvider::synthesize`], [`TtsProvider::provider_id`].
//!   Dyn-compatible (`Box<dyn TtsProvider>`).
//! - [`TtsRequest`] -- a validated synthesis request (text + optional
//!   voice/format/speed).
//! - [`TtsAudio`] -- the synthesized result (encoded bytes + [`AudioFormat`]
//!   + provider id).
//! - [`MockTts`] -- an in-memory, no-network provider for tests and offline
//!   agent development (always available, no feature flag).
//! - [`OpenAiTts`] (feature `openai`) -- an `OpenAI`-compatible `/audio/speech`
//!   HTTP adapter. Works against `OpenAI` itself and any `OpenAI`-API-compatible
//!   TTS gateway (same endpoint shape) via a configurable `api_base`. Needs
//!   only a bearer API key -- no exotic auth flow.
//!
//! ## Real adapters are behind feature flags
//! Same rationale as `familyclaw-channels`: the default build contains
//! **only** the core abstraction + [`MockTts`], so the platform builds and
//! tests without network access or an API key. Enable `openai` to pull in
//! the real HTTP adapter (`reqwest`).
//!
//! ## More providers: see the design doc
//! This crate ships one verified, working adapter ([`OpenAiTts`]). Adding
//! more real backends (`ElevenLabs`, Azure/Edge TTS, a local engine like
//! Piper) is a matter of implementing [`TtsProvider`] behind a new feature
//! flag -- see `docs/design/tts-providers.md` for the extension plan and
//! the reserved-flag convention (mirrors `whatsapp`/`signal` in
//! `familyclaw-channels`).
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode API keys, voice ids, or endpoints.
//! Credentials and destinations are runtime configuration; the types carry
//! only the generic structure.
//!
//! ## Example
//! ```
//! # use familyclaw_tts::{AudioFormat, MockTts, TtsProvider, TtsRequest};
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = MockTts::new();
//! let request = TtsRequest::new("hei maailma")?.with_format(AudioFormat::Wav);
//! let audio = provider.synthesize(request).await?;
//! assert_eq!(audio.provider, "mock");
//! assert_eq!(audio.format, AudioFormat::Wav);
//! # Ok(())
//! # }
//! ```

mod error;
mod mock;
mod provider;

#[cfg(feature = "openai")]
mod openai;

pub use error::{TtsError, TtsResult};
pub use mock::MockTts;
pub use provider::{AudioFormat, TtsAudio, TtsFuture, TtsProvider, TtsRequest};

#[cfg(feature = "openai")]
pub use openai::{
    OpenAiTts, API_KEY_ENV as OPENAI_API_KEY_ENV, DEFAULT_API_BASE as OPENAI_DEFAULT_API_BASE,
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
        let req = TtsRequest::new("hei").expect("valid");
        assert_eq!(req.text(), "hei");
        let mock = MockTts::new();
        assert_eq!(mock.provider_id(), "mock");
        let err = TtsError::invalid_input("x");
        assert!(matches!(err, TtsError::InvalidInput(_)));
        let fmt = AudioFormat::Mp3;
        assert_eq!(fmt.as_str(), "mp3");
    }

    #[tokio::test]
    async fn end_to_end_mock_synthesis() {
        let provider = MockTts::new();
        let request = TtsRequest::new("moi").expect("valid");
        let audio: TtsAudio = provider.synthesize(request).await.expect("mock ok");
        assert_eq!(audio.bytes, b"moi");
        assert_eq!(audio.provider, "mock");
    }
}

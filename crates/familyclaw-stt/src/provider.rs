//! The [`SttProvider`] interface and its request/response types.
//!
//! `SttProvider` is the platform's abstraction for "turn audio into text":
//! - [`SttProvider::transcribe`] takes an [`SttRequest`] and returns an
//!   [`SttTranscript`] (recognized text + optional language),
//! - [`SttProvider::provider_id`] identifies the backend for logging/routing.
//!
//! The interface is **dyn-compatible** (`Box<dyn SttProvider>`), so
//! different providers can be swapped or kept together in a routing table.
//! This is why `transcribe` returns an explicit boxed future
//! ([`SttFuture`]) -- same rationale as
//! [`familyclaw_channels::Channel::send`](../familyclaw_channels/trait.Channel.html#tymethod.send)
//! and [`familyclaw_tts::TtsProvider::synthesize`](../familyclaw_tts/trait.TtsProvider.html#tymethod.synthesize):
//! it avoids a heavy `async-trait` dependency while staying trait-object
//! friendly.

use std::future::Future;
use std::pin::Pin;

use crate::error::{SttError, SttResult};

/// A boxed, sendable future for a provider's [`SttProvider::transcribe`]
/// operation.
pub type SttFuture<'a> = Pin<Box<dyn Future<Output = SttResult<SttTranscript>> + Send + 'a>>;

/// The encoded audio container format an [`SttRequest`]'s input is in.
///
/// Mirrors the subset of input formats accepted by `OpenAI`'s
/// `/audio/transcriptions` (Whisper) endpoint (and `OpenAI`-compatible
/// gateways), since that is the shipped adapter. Providers that only
/// support a subset can reject the unsupported variants via
/// [`SttError::InvalidInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AudioFormat {
    /// MP3 (default -- broadly compatible, small file size).
    #[default]
    Mp3,
    /// Uncompressed WAV (PCM in a RIFF container).
    Wav,
    /// Opus/Ogg container.
    Ogg,
    /// FLAC (lossless).
    Flac,
    /// MPEG-4 audio (AAC in an MP4 container).
    M4a,
    /// `WebM` (Opus/Vorbis in a `WebM` container).
    Webm,
}

impl AudioFormat {
    /// The file extension (no leading dot) this format is identified by --
    /// used to build the multipart filename `OpenAI`-compatible
    /// transcription endpoints infer the input codec from.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::M4a => "m4a",
            Self::Webm => "webm",
        }
    }

    /// The MIME content type for this format, e.g. for the multipart file
    /// part.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Ogg => "audio/ogg",
            Self::Flac => "audio/flac",
            Self::M4a => "audio/mp4",
            Self::Webm => "audio/webm",
        }
    }
}

/// A request to transcribe speech audio into text.
///
/// Constructed via [`SttRequest::new`], which validates that `audio` is
/// non-empty. All other fields are optional and provider-specific defaults
/// apply when unset (e.g. auto language detection, no prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttRequest {
    audio: Vec<u8>,
    format: AudioFormat,
    language: Option<String>,
    prompt: Option<String>,
}

impl SttRequest {
    /// Builds a request to transcribe `audio` encoded as `format`.
    ///
    /// # Errors
    /// [`SttError::InvalidInput`] if `audio` is empty.
    pub fn new(audio: impl Into<Vec<u8>>, format: AudioFormat) -> SttResult<Self> {
        let audio = audio.into();
        if audio.is_empty() {
            return Err(SttError::invalid_input(
                "stt request audio must not be empty",
            ));
        }
        Ok(Self {
            audio,
            format,
            language: None,
            prompt: None,
        })
    }

    /// Sets an ISO-639-1 language hint (e.g. `"fi"`, `"en"`). Improves
    /// accuracy and latency when the spoken language is known; omit for
    /// auto-detection.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Sets an optional prompt to bias transcription (e.g. domain
    /// vocabulary, proper nouns, or the style of a preceding segment).
    #[must_use]
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// The raw encoded audio bytes to transcribe.
    #[must_use]
    pub fn audio(&self) -> &[u8] {
        &self.audio
    }

    /// The container/codec format of [`SttRequest::audio`].
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The language hint, if set.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// The bias prompt, if set.
    #[must_use]
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }
}

impl std::fmt::Debug for SttTranscript {
    /// Standard derive would be fine here (text is expected to be
    /// printable), but this keeps the struct's `Debug` output stable and
    /// explicit -- mirrors `familyclaw_tts::TtsAudio`'s custom `Debug`
    /// (which instead omits the raw bytes).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SttTranscript")
            .field("text", &self.text)
            .field("language", &self.language)
            .field("provider", &self.provider)
            .finish()
    }
}

/// A recognized transcript returned by an [`SttProvider`].
#[derive(Clone, PartialEq, Eq)]
pub struct SttTranscript {
    /// The recognized text.
    pub text: String,
    /// The detected or requested language (ISO-639-1), if the provider
    /// reports one.
    pub language: Option<String>,
    /// The identifier of the provider that produced this transcript
    /// (matches [`SttProvider::provider_id`]).
    pub provider: String,
}

impl SttTranscript {
    /// Builds a new [`SttTranscript`] value.
    #[must_use]
    pub fn new(text: impl Into<String>, provider: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            language: None,
            provider: provider.into(),
        }
    }

    /// Sets the detected/requested language.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// A speech-to-text backend (`OpenAI`/Whisper-compatible HTTP API, a local
/// engine, an in-memory mock, ...).
///
/// Implementations must be `Send + Sync` so providers can be shared between
/// actors/tasks and kept in a `Box<dyn SttProvider>` routing table.
pub trait SttProvider: Send + Sync {
    /// A stable identifier for this provider (e.g. `"openai"`, `"mock"`).
    fn provider_id(&self) -> &str;

    /// Transcribes the given request's audio into text.
    ///
    /// Returns a boxed future ([`SttFuture`]). An error ([`SttResult`])
    /// describes a transport or backend failure; input validation errors
    /// have already been rejected by [`SttRequest::new`]/`with_*`.
    fn transcribe(&self, request: SttRequest) -> SttFuture<'_>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_audio() {
        assert!(matches!(
            SttRequest::new(Vec::new(), AudioFormat::Wav),
            Err(SttError::InvalidInput(_))
        ));
    }

    #[test]
    fn new_accepts_audio_and_defaults() {
        let req = SttRequest::new(b"raw-bytes".to_vec(), AudioFormat::Mp3).expect("valid");
        assert_eq!(req.audio(), b"raw-bytes");
        assert_eq!(req.format(), AudioFormat::Mp3);
        assert_eq!(req.language(), None);
        assert_eq!(req.prompt(), None);
    }

    #[test]
    fn builder_methods_set_fields() {
        let req = SttRequest::new(b"hei".to_vec(), AudioFormat::Wav)
            .expect("valid")
            .with_language("fi")
            .with_prompt("puheenaihe: sää");
        assert_eq!(req.language(), Some("fi"));
        assert_eq!(req.prompt(), Some("puheenaihe: sää"));
    }

    #[test]
    fn audio_format_wire_values_and_content_types() {
        assert_eq!(AudioFormat::Mp3.as_str(), "mp3");
        assert_eq!(AudioFormat::Mp3.content_type(), "audio/mpeg");
        assert_eq!(AudioFormat::Wav.as_str(), "wav");
        assert_eq!(AudioFormat::Wav.content_type(), "audio/wav");
        assert_eq!(AudioFormat::Ogg.as_str(), "ogg");
        assert_eq!(AudioFormat::Flac.as_str(), "flac");
        assert_eq!(AudioFormat::M4a.as_str(), "m4a");
        assert_eq!(AudioFormat::Webm.as_str(), "webm");
    }

    #[test]
    fn stt_transcript_builder_and_debug() {
        let t = SttTranscript::new("hei maailma", "mock").with_language("fi");
        assert_eq!(t.text, "hei maailma");
        assert_eq!(t.language, Some("fi".to_string()));
        let dbg = format!("{t:?}");
        assert!(dbg.contains("hei maailma"));
        assert!(dbg.contains("mock"));
    }

    #[test]
    fn provider_trait_is_dyn_compatible() {
        fn assert_dyn_compatible(_: &dyn SttProvider) {}
        struct Noop;
        impl SttProvider for Noop {
            fn provider_id(&self) -> &'static str {
                "noop"
            }
            fn transcribe(&self, _request: SttRequest) -> SttFuture<'_> {
                Box::pin(async move { Ok(SttTranscript::new(String::new(), "noop")) })
            }
        }
        let n = Noop;
        assert_dyn_compatible(&n);
    }
}

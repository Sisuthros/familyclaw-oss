//! The [`TtsProvider`] interface and its request/response types.
//!
//! `TtsProvider` is the platform's abstraction for "turn text into audio":
//! - [`TtsProvider::synthesize`] takes a [`TtsRequest`] and returns a
//!   [`TtsAudio`] (raw encoded bytes + format),
//! - [`TtsProvider::provider_id`] identifies the backend for logging/routing.
//!
//! The interface is **dyn-compatible** (`Box<dyn TtsProvider>`), so
//! different providers can be swapped or kept together in a routing table.
//! This is why `synthesize` returns an explicit boxed future
//! ([`TtsFuture`]) -- same rationale as
//! [`familyclaw_channels::Channel::send`](../familyclaw_channels/trait.Channel.html#tymethod.send):
//! it avoids a heavy `async-trait` dependency while staying trait-object
//! friendly.

use std::future::Future;
use std::pin::Pin;

use crate::error::{TtsError, TtsResult};

/// A boxed, sendable future for a provider's [`TtsProvider::synthesize`]
/// operation.
pub type TtsFuture<'a> = Pin<Box<dyn Future<Output = TtsResult<TtsAudio>> + Send + 'a>>;

/// The encoded audio container format a provider returns.
///
/// Mirrors the `response_format` values accepted by `OpenAI`'s
/// `/audio/speech` endpoint (and `OpenAI`-compatible gateways), since that is
/// the shipped adapter. Providers that only support a subset can reject the
/// unsupported variants via [`TtsError::InvalidInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AudioFormat {
    /// MP3 (default -- broadly compatible, small file size).
    #[default]
    Mp3,
    /// Uncompressed WAV (PCM in a RIFF container).
    Wav,
    /// Opus (low-latency streaming).
    Opus,
    /// AAC.
    Aac,
    /// FLAC (lossless).
    Flac,
    /// Raw 24kHz 16-bit little-endian PCM, no container.
    Pcm,
}

impl AudioFormat {
    /// The wire value the `OpenAI`-compatible `response_format` field expects.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::Opus => "opus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Pcm => "pcm",
        }
    }

    /// The MIME content type for this format, e.g. for HTTP responses or
    /// writing a file with the right extension inferred.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Pcm => "audio/L16",
        }
    }
}

/// A request to synthesize speech from text.
///
/// Constructed via [`TtsRequest::new`], which validates that `text` is
/// non-empty. All other fields are optional and provider-specific defaults
/// apply when unset (e.g. [`AudioFormat::default`], a provider's default
/// voice).
#[derive(Debug, Clone, PartialEq)]
pub struct TtsRequest {
    text: String,
    voice: Option<String>,
    format: AudioFormat,
    speed: Option<f32>,
}

/// The valid range for [`TtsRequest::with_speed`], matching the range
/// accepted by `OpenAI`'s `/audio/speech` `speed` parameter.
const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

impl TtsRequest {
    /// Builds a request to speak `text`.
    ///
    /// # Errors
    /// [`TtsError::InvalidInput`] if `text` is empty or whitespace-only.
    pub fn new(text: impl Into<String>) -> TtsResult<Self> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(TtsError::invalid_input(
                "tts request text must not be empty",
            ));
        }
        Ok(Self {
            text,
            voice: None,
            format: AudioFormat::default(),
            speed: None,
        })
    }

    /// Sets the voice identifier (provider-specific, e.g. `"alloy"`).
    #[must_use]
    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = Some(voice.into());
        self
    }

    /// Sets the desired output [`AudioFormat`].
    #[must_use]
    pub fn with_format(mut self, format: AudioFormat) -> Self {
        self.format = format;
        self
    }

    /// Sets the playback speed multiplier.
    ///
    /// # Errors
    /// [`TtsError::InvalidInput`] if `speed` is outside `0.25..=4.0`.
    pub fn with_speed(mut self, speed: f32) -> TtsResult<Self> {
        if !SPEED_RANGE.contains(&speed) {
            return Err(TtsError::invalid_input(format!(
                "tts speed must be within {SPEED_RANGE:?}, got {speed}"
            )));
        }
        self.speed = Some(speed);
        Ok(self)
    }

    /// The text to speak.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The requested voice, if set.
    #[must_use]
    pub fn voice(&self) -> Option<&str> {
        self.voice.as_deref()
    }

    /// The requested output format.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// The requested playback speed, if set.
    #[must_use]
    pub fn speed(&self) -> Option<f32> {
        self.speed
    }
}

/// Synthesized speech audio returned by a [`TtsProvider`].
#[derive(Clone, PartialEq, Eq)]
pub struct TtsAudio {
    /// The encoded audio bytes (format described by [`TtsAudio::format`]).
    pub bytes: Vec<u8>,
    /// The container/codec format of `bytes`.
    pub format: AudioFormat,
    /// The identifier of the provider that produced this audio (matches
    /// [`TtsProvider::provider_id`]).
    pub provider: String,
}

impl std::fmt::Debug for TtsAudio {
    /// Omits the raw audio bytes (can be large / non-printable) and shows
    /// only their length.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TtsAudio")
            .field("bytes_len", &self.bytes.len())
            .field("format", &self.format)
            .field("provider", &self.provider)
            .finish()
    }
}

impl TtsAudio {
    /// Builds a new [`TtsAudio`] value.
    #[must_use]
    pub fn new(bytes: Vec<u8>, format: AudioFormat, provider: impl Into<String>) -> Self {
        Self {
            bytes,
            format,
            provider: provider.into(),
        }
    }

    /// The MIME content type of [`TtsAudio::bytes`] (delegates to
    /// [`AudioFormat::content_type`]).
    #[must_use]
    pub fn content_type(&self) -> &'static str {
        self.format.content_type()
    }
}

/// A text-to-speech backend (`OpenAI`-compatible HTTP API, a local engine,
/// an in-memory mock, ...).
///
/// Implementations must be `Send + Sync` so providers can be shared between
/// actors/tasks and kept in a `Box<dyn TtsProvider>` routing table.
pub trait TtsProvider: Send + Sync {
    /// A stable identifier for this provider (e.g. `"openai"`, `"mock"`).
    fn provider_id(&self) -> &str;

    /// Synthesizes speech audio for the given request.
    ///
    /// Returns a boxed future ([`TtsFuture`]). An error ([`TtsResult`])
    /// describes a transport or backend failure; input validation errors
    /// have already been rejected by [`TtsRequest::new`]/`with_*`.
    fn synthesize(&self, request: TtsRequest) -> TtsFuture<'_>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_text() {
        assert!(matches!(
            TtsRequest::new(""),
            Err(TtsError::InvalidInput(_))
        ));
        assert!(matches!(
            TtsRequest::new("   "),
            Err(TtsError::InvalidInput(_))
        ));
    }

    #[test]
    fn new_accepts_text_and_defaults() {
        let req = TtsRequest::new("hei maailma").expect("valid");
        assert_eq!(req.text(), "hei maailma");
        assert_eq!(req.voice(), None);
        assert_eq!(req.format(), AudioFormat::Mp3);
        assert_eq!(req.speed(), None);
    }

    #[test]
    fn builder_methods_set_fields() {
        let req = TtsRequest::new("hei")
            .expect("valid")
            .with_voice("alloy")
            .with_format(AudioFormat::Wav)
            .with_speed(1.5)
            .expect("valid speed");
        assert_eq!(req.voice(), Some("alloy"));
        assert_eq!(req.format(), AudioFormat::Wav);
        assert_eq!(req.speed(), Some(1.5));
    }

    #[test]
    fn with_speed_rejects_out_of_range() {
        let req = TtsRequest::new("hei").expect("valid");
        assert!(matches!(
            req.clone().with_speed(0.1),
            Err(TtsError::InvalidInput(_))
        ));
        assert!(matches!(
            req.with_speed(4.1),
            Err(TtsError::InvalidInput(_))
        ));
    }

    #[test]
    fn audio_format_wire_values_and_content_types() {
        assert_eq!(AudioFormat::Mp3.as_str(), "mp3");
        assert_eq!(AudioFormat::Mp3.content_type(), "audio/mpeg");
        assert_eq!(AudioFormat::Wav.as_str(), "wav");
        assert_eq!(AudioFormat::Wav.content_type(), "audio/wav");
        assert_eq!(AudioFormat::Opus.as_str(), "opus");
        assert_eq!(AudioFormat::Aac.as_str(), "aac");
        assert_eq!(AudioFormat::Flac.as_str(), "flac");
        assert_eq!(AudioFormat::Pcm.as_str(), "pcm");
    }

    #[test]
    fn tts_audio_debug_omits_raw_bytes() {
        let audio = TtsAudio::new(vec![1, 2, 3, 4, 5], AudioFormat::Mp3, "mock");
        let dbg = format!("{audio:?}");
        assert!(dbg.contains("bytes_len: 5"));
        assert!(!dbg.contains('\u{1}')); // raw byte 1 not printed literally
        assert_eq!(audio.content_type(), "audio/mpeg");
    }

    #[test]
    fn provider_trait_is_dyn_compatible() {
        fn assert_dyn_compatible(_: &dyn TtsProvider) {}
        struct Noop;
        impl TtsProvider for Noop {
            fn provider_id(&self) -> &'static str {
                "noop"
            }
            fn synthesize(&self, request: TtsRequest) -> TtsFuture<'_> {
                Box::pin(async move { Ok(TtsAudio::new(Vec::new(), request.format(), "noop")) })
            }
        }
        let n = Noop;
        assert_dyn_compatible(&n);
    }
}

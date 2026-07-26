//! STT layer error types.
//!
//! [`SttError`] covers input validation and the provider adapter's
//! transport/backend failures. It converts into the platform's centralized
//! [`FamilyClawError`] type via a [`From`] implementation (mapped to
//! [`FamilyClawError::Llm`] -- a STT call is, like an LLM call, an external
//! generative-AI API request), so STT errors flow through the same error
//! path as the rest of the platform.
//!
//! The production path does NOT use `unwrap()`/`expect()`/`panic!()` -- all
//! STT errors flow through the [`Result`] type.

use familyclaw_core::FamilyClawError;
use thiserror::Error;

/// A speech-to-text layer error.
///
/// `#[non_exhaustive]` so new variants can be added later without breaking
/// downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SttError {
    /// The given input (empty audio, unsupported format, ...) was invalid.
    #[error("invalid stt input: {0}")]
    InvalidInput(String),

    /// The request to the provider could not be completed (network error,
    /// timeout, connection refused, ...). No HTTP status was received.
    #[error("stt request to provider '{provider}' failed: {reason}")]
    Request {
        /// The provider's identifier (e.g. `"openai"`).
        provider: String,
        /// A human-readable reason.
        reason: String,
    },

    /// The provider responded, but reported an error (non-2xx HTTP status,
    /// or a malformed/empty transcript payload).
    #[error("stt provider '{provider}' returned an error (status {status}): {reason}")]
    Backend {
        /// The provider's identifier.
        provider: String,
        /// The HTTP status code returned by the provider.
        status: u16,
        /// The reason reported by the provider (response body / parse error).
        reason: String,
    },
}

impl SttError {
    /// Builds a [`SttError::InvalidInput`] variant.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Builds a [`SttError::Request`] variant.
    pub fn request(provider: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Request {
            provider: provider.into(),
            reason: reason.into(),
        }
    }

    /// Builds a [`SttError::Backend`] variant.
    pub fn backend(provider: impl Into<String>, status: u16, reason: impl Into<String>) -> Self {
        Self::Backend {
            provider: provider.into(),
            status,
            reason: reason.into(),
        }
    }
}

impl From<SttError> for FamilyClawError {
    /// A STT error is classified at the platform level as an LLM-family
    /// error: both are external generative-AI API calls with the same
    /// transport/backend failure shape.
    fn from(err: SttError) -> Self {
        FamilyClawError::llm(err.to_string())
    }
}

/// The STT layer's standard result type.
pub type SttResult<T> = std::result::Result<T, SttError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_variant_and_message() {
        assert!(matches!(
            SttError::invalid_input("empty audio"),
            SttError::InvalidInput(_)
        ));
        assert_eq!(
            SttError::invalid_input("empty audio").to_string(),
            "invalid stt input: empty audio"
        );
        assert_eq!(
            SttError::request("openai", "connection refused").to_string(),
            "stt request to provider 'openai' failed: connection refused"
        );
        assert_eq!(
            SttError::backend("openai", 401, "invalid api key").to_string(),
            "stt provider 'openai' returned an error (status 401): invalid api key"
        );
    }

    #[test]
    fn converts_into_familyclaw_llm_error() {
        let err: FamilyClawError = SttError::backend("openai", 500, "boom").into();
        assert!(matches!(err, FamilyClawError::Llm(_)));
        assert!(err.to_string().contains("stt provider 'openai'"));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<SttError>();
    }
}

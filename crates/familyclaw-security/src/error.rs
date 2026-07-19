//! Error types for the security layer.
//!
//! All failures in this crate flow through the [`SecurityError`] type —
//! **no** `unwrap()`/`expect()`/`panic!()` on the production path. The type
//! converts into [`familyclaw_core::FamilyClawError`] via a [`From`]
//! implementation, so security errors can flow through the platform's
//! centralized error type.

use thiserror::Error;

use familyclaw_core::FamilyClawError;

/// The security layer's error type.
///
/// Covers the error classes for identity anchors, tamper detection, and
/// [`crate::HumanCorrection`]. `#[non_exhaustive]` so new variants can be
/// added without breaking downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SecurityError {
    /// The given content was invalid (e.g. empty SOUL content for an anchor).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The given hash string was not a valid hexadecimal SHA-256 digest
    /// (wrong length or non-hex characters).
    #[error("invalid hash: {0}")]
    InvalidHash(String),

    /// JSON serialization or parsing failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl SecurityError {
    /// Constructs a [`SecurityError::InvalidInput`] variant.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Constructs a [`SecurityError::InvalidHash`] variant.
    pub fn invalid_hash(msg: impl Into<String>) -> Self {
        Self::InvalidHash(msg.into())
    }
}

impl From<SecurityError> for FamilyClawError {
    fn from(err: SecurityError) -> Self {
        match err {
            // Keep serde as the platform's natural variant for it.
            SecurityError::Serde(serde) => FamilyClawError::Serde(serde),
            // The rest are input/validation errors.
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

/// The security crate's standard result type.
pub type Result<T> = std::result::Result<T, SecurityError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_input_constructor_formats() {
        let err = SecurityError::invalid_input("empty soul");
        assert_eq!(err.to_string(), "invalid input: empty soul");
    }

    #[test]
    fn invalid_hash_constructor_formats() {
        let err = SecurityError::invalid_hash("odd length");
        assert_eq!(err.to_string(), "invalid hash: odd length");
    }

    #[test]
    fn serde_converts_into_core_serde() {
        let parse = serde_json::from_str::<serde_json::Value>("{bad").expect_err("must fail");
        let sec: SecurityError = parse.into();
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::Serde(_)));
    }

    #[test]
    fn invalid_input_converts_into_core_invalid_input() {
        let sec = SecurityError::invalid_input("boom");
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn invalid_hash_converts_into_core_invalid_input() {
        let sec = SecurityError::invalid_hash("nope");
        let core: FamilyClawError = sec.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<SecurityError>();
    }
}

//! Error types for the entire `FamilyClaw` platform.
//!
//! One centralized error type, [`FamilyClawError`], covers all layers
//! (config, IO, serialization, bus, memory). Crates can wrap their own
//! errors into this type or define their own types that convert into it via
//! [`From`] implementations. The production code path does NOT use
//! `unwrap()`/`expect()`/`panic!()` — all errors flow through the
//! [`Result`] type.

use std::io;

use thiserror::Error;

/// The centralized error type for the `FamilyClaw` platform.
///
/// Each variant corresponds to one error category the platform can
/// encounter. The type is `#[non_exhaustive]` so new variants can be added
/// later without breaking downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FamilyClawError {
    /// Configuration loading or validation failed.
    #[error("config error: {0}")]
    Config(String),

    /// File or network IO failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// JSON serialization or parsing failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Resonance Bus-level error (actor messaging, channels, mailbox).
    #[error("bus error: {0}")]
    Bus(String),

    /// Memory substrate error (Eternal Thread, vectors, decay).
    #[error("memory error: {0}")]
    Memory(String),

    /// The requested resource (agent, family, message) was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// The given input was invalid (validation error).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The LLM request failed (network or API error).
    #[error("llm error: {0}")]
    Llm(String),

    /// Sandbox execution failed (WASM, fuel, capability).
    #[error("sandbox error: {0}")]
    Sandbox(String),
}

impl FamilyClawError {
    /// Builds a [`FamilyClawError::Config`] variant from any value
    /// convertible into a string.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Builds a [`FamilyClawError::Bus`] variant.
    pub fn bus(msg: impl Into<String>) -> Self {
        Self::Bus(msg.into())
    }

    /// Builds a [`FamilyClawError::Memory`] variant.
    pub fn memory(msg: impl Into<String>) -> Self {
        Self::Memory(msg.into())
    }

    /// Builds a [`FamilyClawError::NotFound`] variant.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Builds a [`FamilyClawError::InvalidInput`] variant.
    pub fn invalid_input(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    /// Builds a [`FamilyClawError::Llm`] variant.
    pub fn llm(msg: impl Into<String>) -> Self {
        Self::Llm(msg.into())
    }

    /// Builds a [`FamilyClawError::Sandbox`] variant.
    pub fn sandbox(msg: impl Into<String>) -> Self {
        Self::Sandbox(msg.into())
    }
}

/// The platform's standard result type: [`std::result::Result`] whose
/// error is always [`FamilyClawError`].
pub type Result<T> = std::result::Result<T, FamilyClawError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_constructor_sets_variant_and_message() {
        let err = FamilyClawError::config("missing key");
        assert!(matches!(err, FamilyClawError::Config(_)));
        assert_eq!(err.to_string(), "config error: missing key");
    }

    #[test]
    fn bus_memory_not_found_invalid_constructors() {
        assert_eq!(
            FamilyClawError::bus("mailbox closed").to_string(),
            "bus error: mailbox closed"
        );
        assert_eq!(
            FamilyClawError::memory("decay failed").to_string(),
            "memory error: decay failed"
        );
        assert_eq!(
            FamilyClawError::not_found("agent_x").to_string(),
            "not found: agent_x"
        );
        assert_eq!(
            FamilyClawError::invalid_input("empty name").to_string(),
            "invalid input: empty name"
        );
    }

    #[test]
    fn io_error_converts_via_from() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "no file");
        let err: FamilyClawError = io_err.into();
        assert!(matches!(err, FamilyClawError::Io(_)));
        assert!(err.to_string().starts_with("io error:"));
    }

    #[test]
    fn serde_error_converts_via_from() {
        let parse_err = serde_json::from_str::<serde_json::Value>("{ not json")
            .expect_err("malformed json must fail");
        let err: FamilyClawError = parse_err.into();
        assert!(matches!(err, FamilyClawError::Serde(_)));
        assert!(err.to_string().starts_with("serde error:"));
    }

    #[test]
    fn result_alias_is_usable() {
        fn maybe(fail: bool) -> Result<u8> {
            if fail {
                Err(FamilyClawError::config("boom"))
            } else {
                Ok(42)
            }
        }
        assert_eq!(maybe(false).expect("ok"), 42);
        assert!(maybe(true).is_err());
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<FamilyClawError>();
    }

    #[test]
    fn sandbox_constructor_sets_variant_and_message() {
        let err = FamilyClawError::sandbox("no wasmtime");
        assert!(matches!(err, FamilyClawError::Sandbox(_)));
        assert_eq!(err.to_string(), "sandbox error: no wasmtime");
    }
}

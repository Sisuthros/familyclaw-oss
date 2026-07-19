//! Error types for the benchmark harness.
//!
//! All harness failures flow through the [`BenchError`] and [`Result`]
//! types — the production path never uses `unwrap()`/`expect()`/
//! `panic!()`. [`BenchError`] wraps the platform's core crate error
//! ([`familyclaw_core::FamilyClawError`]) as well as durable/dream-layer
//! errors, so scenarios can propagate them with the `?` operator.

use thiserror::Error;

/// The benchmark harness's central error type.
///
/// `#[non_exhaustive]` so new variants can be added later without breaking
/// downstream scenarios.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BenchError {
    /// The subject (the system under benchmark) failed during an operation.
    #[error("subject error: {0}")]
    Subject(String),

    /// A scenario run failed.
    #[error("scenario error: {0}")]
    Scenario(String),

    /// A metric calculation received invalid input.
    #[error("metric error: {0}")]
    Metric(String),

    /// Scorecard serialization or write failed.
    #[error("scorecard error: {0}")]
    Scorecard(String),

    /// A platform core error (config, IO, memory, bus).
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),

    /// A durable-substrate error (journal, replay).
    #[error("durable error: {0}")]
    Durable(#[from] familyclaw_durable::DurableError),

    /// JSON serialization or parsing failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// File or process IO failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl BenchError {
    /// Builds a [`BenchError::Subject`] variant.
    pub fn subject(msg: impl Into<String>) -> Self {
        Self::Subject(msg.into())
    }

    /// Builds a [`BenchError::Scenario`] variant.
    pub fn scenario(msg: impl Into<String>) -> Self {
        Self::Scenario(msg.into())
    }

    /// Builds a [`BenchError::Metric`] variant.
    pub fn metric(msg: impl Into<String>) -> Self {
        Self::Metric(msg.into())
    }

    /// Builds a [`BenchError::Scorecard`] variant.
    pub fn scorecard(msg: impl Into<String>) -> Self {
        Self::Scorecard(msg.into())
    }
}

/// The harness's standard result type: [`std::result::Result`] whose error
/// is always [`BenchError`].
pub type Result<T> = std::result::Result<T, BenchError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_variant_and_message() {
        assert!(matches!(BenchError::subject("x"), BenchError::Subject(_)));
        assert!(matches!(BenchError::scenario("x"), BenchError::Scenario(_)));
        assert!(matches!(BenchError::metric("x"), BenchError::Metric(_)));
        assert!(matches!(
            BenchError::scorecard("x"),
            BenchError::Scorecard(_)
        ));
    }

    #[test]
    fn core_error_converts_via_from() {
        let err: BenchError = familyclaw_core::FamilyClawError::config("boom").into();
        assert!(matches!(err, BenchError::Core(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<BenchError>();
    }
}

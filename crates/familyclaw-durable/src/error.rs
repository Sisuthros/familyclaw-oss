//! Error types for the durable substrate.
//!
//! All failures in this crate flow through the [`DurableError`] type —
//! **no** `unwrap()`/`expect()`/`panic!()` on the production path. The type
//! converts into [`familyclaw_core::FamilyClawError`] via a [`From`]
//! implementation, so durable errors can flow through the platform's
//! centralized error type.

use thiserror::Error;

use familyclaw_core::FamilyClawError;

/// Error type for the durable substrate.
///
/// `#[non_exhaustive]` so new variants can be added without breaking
/// downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DurableError {
    /// IO failure in the journal's backing storage (open, write, fsync,
    /// read).
    #[error("journal io error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization or parsing of a journal line failed.
    #[error("journal serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Replay detected nondeterminism: the recorded step does not match
    /// the step currently being executed (e.g. a different name at the
    /// same sequence position).
    ///
    /// This is the hardest invariant of the durable-execution model: the
    /// code must produce the same steps in the same order on every run.
    #[error(
        "nondeterministic replay at step #{index}: expected step {expected:?}, found {found:?}"
    )]
    NondeterministicReplay {
        /// The step's sequence order number (0-based).
        index: u64,
        /// The step name expected by the replay code at this position.
        expected: String,
        /// The step name found in the journal at this position.
        found: String,
    },

    /// A journal line was corrupt and could not be parsed into a
    /// meaningful entry (e.g. a JSONL line truncated by a crash).
    #[error("corrupt journal entry at line {line}: {reason}")]
    CorruptEntry {
        /// 1-based line number in the backing file.
        line: u64,
        /// Human-readable reason why the line was rejected.
        reason: String,
    },

    /// The closure run inside a step returned an error. The error is
    /// retained as a string because the durable log stores the error
    /// outcome as text.
    #[error("step '{step}' failed: {message}")]
    StepFailed {
        /// The step's logical name.
        step: String,
        /// The error message returned by the closure.
        message: String,
    },

    /// Timeline fork failed — e.g. the cut point is outside the log's
    /// step count, or the target journal was not empty.
    ///
    /// Fork is **fail-closed**: in an ambiguous situation, the fork
    /// refuses rather than silently producing a malformed timeline.
    #[error("invalid timeline fork: {reason}")]
    InvalidFork {
        /// Human-readable reason why the fork was rejected.
        reason: String,
    },
}

impl DurableError {
    /// Builds a [`DurableError::StepFailed`] variant.
    pub fn step_failed(step: impl Into<String>, message: impl Into<String>) -> Self {
        Self::StepFailed {
            step: step.into(),
            message: message.into(),
        }
    }

    /// Builds a [`DurableError::CorruptEntry`] variant.
    pub fn corrupt(line: u64, reason: impl Into<String>) -> Self {
        Self::CorruptEntry {
            line,
            reason: reason.into(),
        }
    }

    /// Builds a [`DurableError::InvalidFork`] variant.
    pub fn invalid_fork(reason: impl Into<String>) -> Self {
        Self::InvalidFork {
            reason: reason.into(),
        }
    }
}

impl From<DurableError> for FamilyClawError {
    fn from(err: DurableError) -> Self {
        match err {
            // Preserve IO/serde as the platform's native variants.
            DurableError::Io(io) => FamilyClawError::Io(io),
            DurableError::Serde(serde) => FamilyClawError::Serde(serde),
            // Map the rest to memory-layer errors (durable = the memory
            // substrate) while preserving the human-readable message.
            other => FamilyClawError::memory(other.to_string()),
        }
    }
}

/// The standard result type for the durable crate.
pub type Result<T> = std::result::Result<T, DurableError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_failed_constructor_formats() {
        let err = DurableError::step_failed("render", "out of memory");
        assert_eq!(err.to_string(), "step 'render' failed: out of memory");
    }

    #[test]
    fn corrupt_constructor_formats() {
        let err = DurableError::corrupt(7, "truncated json");
        assert_eq!(
            err.to_string(),
            "corrupt journal entry at line 7: truncated json"
        );
    }

    #[test]
    fn nondeterministic_replay_formats() {
        let err = DurableError::NondeterministicReplay {
            index: 2,
            expected: "b".to_string(),
            found: "c".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "nondeterministic replay at step #2: expected step \"b\", found \"c\""
        );
    }

    #[test]
    fn io_converts_into_core_io() {
        let io = std::io::Error::other("disk full");
        let durable: DurableError = io.into();
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Io(_)));
    }

    #[test]
    fn serde_converts_into_core_serde() {
        let parse = serde_json::from_str::<serde_json::Value>("{bad").expect_err("must fail");
        let durable: DurableError = parse.into();
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Serde(_)));
    }

    #[test]
    fn non_io_converts_into_core_memory() {
        let durable = DurableError::step_failed("s", "boom");
        let core: FamilyClawError = durable.into();
        assert!(matches!(core, FamilyClawError::Memory(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<DurableError>();
    }
}

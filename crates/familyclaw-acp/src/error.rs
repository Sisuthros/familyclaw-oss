//! ACP error types.
//!
//! All errors the ACP client can encounter: spawn, JSON, I/O, timeout.

use std::path::PathBuf;

/// ACP client error types.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// Spawning the binary failed.
    #[error("failed to spawn ACP agent '{binary}': {reason}")]
    Spawn {
        /// The binary that was attempted to start.
        binary: PathBuf,
        /// Reason.
        reason: String,
    },

    /// JSON serialization/deserialization failed.
    #[error("ACP JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O error on the stdin/stdout connection.
    #[error("ACP I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The agent timed out.
    #[error("ACP agent timeout after {secs}s: {agent}")]
    Timeout {
        /// The agent's name.
        agent: String,
        /// Timeout limit in seconds.
        secs: u64,
    },

    /// The agent returned an invalid response.
    #[error("ACP unexpected response from '{agent}': {detail}")]
    UnexpectedResponse {
        /// The agent's name.
        agent: String,
        /// Details.
        detail: String,
    },

    /// The agent crashed (exit code != 0).
    #[error("ACP agent '{agent}' crashed with exit code {code}")]
    Crash {
        /// The agent's name.
        agent: String,
        /// The process's exit code.
        code: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_display_includes_binary_and_reason() {
        let err = AcpError::Spawn {
            binary: PathBuf::from("agent_a"),
            reason: "not found".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "failed to spawn ACP agent 'agent_a': not found"
        );
    }

    #[test]
    fn json_display_wraps_inner_message() {
        // Produce a real serde_json::Error via the From conversion.
        let json_err = serde_json::from_str::<i32>("not json").unwrap_err();
        let inner = json_err.to_string();
        let err: AcpError = json_err.into();
        assert!(matches!(err, AcpError::Json(_)));
        assert_eq!(err.to_string(), format!("ACP JSON error: {inner}"));
    }

    #[test]
    fn io_display_wraps_inner_message() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed");
        let inner = io_err.to_string();
        let err: AcpError = io_err.into();
        assert!(matches!(err, AcpError::Io(_)));
        assert_eq!(err.to_string(), format!("ACP I/O error: {inner}"));
    }

    #[test]
    fn timeout_display_includes_agent_and_secs() {
        let err = AcpError::Timeout {
            agent: "agent_a".to_string(),
            secs: 120,
        };
        assert_eq!(err.to_string(), "ACP agent timeout after 120s: agent_a");
    }

    #[test]
    fn unexpected_response_display_includes_agent_and_detail() {
        let err = AcpError::UnexpectedResponse {
            agent: "agent_a".to_string(),
            detail: "garbled output".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "ACP unexpected response from 'agent_a': garbled output"
        );
    }

    #[test]
    fn crash_display_includes_agent_and_code() {
        let err = AcpError::Crash {
            agent: "agent_a".to_string(),
            code: 137,
        };
        assert_eq!(
            err.to_string(),
            "ACP agent 'agent_a' crashed with exit code 137"
        );
    }

    #[test]
    fn from_serde_json_error_yields_json_variant() {
        let json_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err = AcpError::from(json_err);
        assert!(matches!(err, AcpError::Json(_)));
    }

    #[test]
    fn from_io_error_yields_io_variant() {
        let io_err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let err = AcpError::from(io_err);
        assert!(matches!(err, AcpError::Io(_)));
    }

    #[test]
    fn question_mark_propagates_io_error_via_from() {
        // Verifies that `?` routes io::Error → AcpError::Io.
        fn inner() -> Result<(), AcpError> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))?;
            Ok(())
        }
        let err = inner().expect_err("should be an error");
        assert!(matches!(err, AcpError::Io(_)));
    }
}

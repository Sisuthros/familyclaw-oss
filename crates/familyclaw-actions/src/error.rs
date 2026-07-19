//! The action stack's crate-local error type [`ActionError`].
//!
//! `familyclaw-core` provides a centralized [`FamilyClawError`] type for the
//! whole platform. This crate defines its own, more fine-grained error type
//! for the action stack's (observe→plan→approve→execute→verify→prove→remember→report)
//! special cases, and converts it to the centralized type when needed via
//! [`From`] implementations in both directions.
//!
//! The production path does NOT use `unwrap()`/`expect()`/`panic!()` — all
//! errors flow through the [`Result`] type.

use familyclaw_core::FamilyClawError;
use thiserror::Error;

/// The action stack's error type.
///
/// `#[non_exhaustive]` so that new variants can be added later without
/// breaking downstream code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ActionError {
    /// Skill manifest parsing failed (e.g. invalid JSON).
    #[error("manifest parse error: {0}")]
    ManifestParse(String),

    /// Skill manifest validation failed (missing field, invalid value).
    #[error("manifest validation error: {0}")]
    ManifestValidation(String),

    /// A value that looks like a secret was detected in the manifest (not allowed).
    #[error("secret detected in manifest: {0}")]
    SecretInManifest(String),

    /// Verification of an external skill's Ed25519 signature failed.
    #[error("skill signature invalid: {0}")]
    SignatureInvalid(String),

    /// The referenced skill was not found in the registry.
    #[error("unknown skill: {0}")]
    UnknownSkill(String),

    /// The referenced entity (e.g. a task) was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// The state machine transition was not legal.
    #[error("illegal transition: {0}")]
    IllegalTransition(String),

    /// The required approval is missing entirely.
    #[error("approval missing: {0}")]
    ApprovalMissing(String),

    /// The approval has expired (TTL exceeded).
    #[error("approval expired: {0}")]
    ApprovalExpired(String),

    /// The approval has already been used (replay prevented, nonce consumed).
    #[error("approval reused: {0}")]
    ApprovalReused(String),

    /// The approval's payload hash does not match the payload to be executed.
    #[error("approval payload mismatch: {0}")]
    ApprovalPayloadMismatch(String),

    /// Policy blocked the action.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// Execution of the action failed.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// Building or validating the proof bundle failed.
    #[error("proof error: {0}")]
    Proof(String),

    /// The MCP tool was not found.
    #[error("mcp unknown tool: {0}")]
    McpUnknownTool(String),

    /// Use of the MCP tool was denied (e.g. missing capability).
    #[error("mcp denied: {0}")]
    McpDenied(String),

    /// Wraps the underlying centralized [`FamilyClawError`] error
    /// (e.g. IO, serde, config) without loss of information.
    #[error(transparent)]
    Core(#[from] FamilyClawError),
}

/// The action stack's standard result type: [`std::result::Result`] whose
/// error is always [`ActionError`].
pub type Result<T> = std::result::Result<T, ActionError>;

impl From<ActionError> for FamilyClawError {
    /// Converts the action stack's error back into the centralized type.
    ///
    /// Errors already wrapped from the centralized type ([`ActionError::Core`])
    /// are unwrapped as-is; other variants are mapped to the
    /// [`FamilyClawError::InvalidInput`] variant, preserving the message.
    fn from(err: ActionError) -> Self {
        match err {
            ActionError::Core(inner) => inner,
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_format_with_context() {
        assert_eq!(
            ActionError::UnknownSkill("skill_a".into()).to_string(),
            "unknown skill: skill_a"
        );
        assert_eq!(
            ActionError::PolicyDenied("not allowed".into()).to_string(),
            "policy denied: not allowed"
        );
        assert_eq!(
            ActionError::ApprovalExpired("ttl 0".into()).to_string(),
            "approval expired: ttl 0"
        );
    }

    #[test]
    fn core_error_converts_into_action_error() {
        let core = FamilyClawError::not_found("agent_a");
        let action: ActionError = core.into();
        assert!(matches!(action, ActionError::Core(_)));
    }

    #[test]
    fn action_error_converts_into_core_error() {
        let action = ActionError::McpDenied("no capability".into());
        let core: FamilyClawError = action.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
        assert!(core.to_string().contains("mcp denied"));
    }

    #[test]
    fn core_wrapped_unwraps_back_to_core() {
        let original = FamilyClawError::memory("decay failed");
        let action: ActionError = ActionError::Core(original);
        let core: FamilyClawError = action.into();
        assert!(matches!(core, FamilyClawError::Memory(_)));
    }

    #[test]
    fn result_alias_is_usable() {
        fn maybe(fail: bool) -> Result<u8> {
            if fail {
                Err(ActionError::ExecutionFailed("boom".into()))
            } else {
                Ok(7)
            }
        }
        assert_eq!(maybe(false).expect("ok"), 7);
        assert!(maybe(true).is_err());
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ActionError>();
    }
}

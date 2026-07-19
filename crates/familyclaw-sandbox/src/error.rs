//! Sandbox-specific error types.
//!
//! The sandbox has its own error type [`SandboxError`] which carries the
//! precise error categories of the sandbox domain (fuel exhaustion,
//! capability violation, missing backend). It converts, when needed, into
//! the platform's centralized [`familyclaw_core::FamilyClawError`] type via
//! a [`From`] implementation.

use thiserror::Error;

/// The sandbox's error type.
///
/// `#[non_exhaustive]` so that new variants can be added without breaking
/// downstream code (e.g. new backends or trust classes).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// Fuel ran out mid-execution.
    #[error("fuel exhausted: budget {budget}, required {required}")]
    FuelExhausted {
        /// The fuel budget that was available.
        budget: u64,
        /// The consumption that would have been required (exceeded the budget).
        required: u64,
    },

    /// The executed code violated a capability restriction, or the
    /// capability set was invalid.
    #[error("capability violation: {0}")]
    Capability(String),

    /// The requested operation is not implemented in this backend.
    ///
    /// The default [`NoopSandbox`](crate::NoopSandbox) returns this: real
    /// execution requires the `wasmtime` feature.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// Sandbox initialization failed (e.g. WASM module loading or linking).
    #[error("sandbox setup failed: {0}")]
    Setup(String),

    /// Execution failed for another reason (e.g. a WASM trap, other than fuel).
    #[error("execution failed: {0}")]
    Execution(String),
}

impl SandboxError {
    /// Builds a [`SandboxError::FuelExhausted`] variant.
    #[must_use]
    pub const fn fuel_exhausted(budget: u64, required: u64) -> Self {
        Self::FuelExhausted { budget, required }
    }

    /// Builds a [`SandboxError::Capability`] variant.
    pub fn capability(msg: impl Into<String>) -> Self {
        Self::Capability(msg.into())
    }

    /// Builds a [`SandboxError::NotImplemented`] variant.
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self::NotImplemented(msg.into())
    }

    /// Builds a [`SandboxError::Setup`] variant.
    pub fn setup(msg: impl Into<String>) -> Self {
        Self::Setup(msg.into())
    }

    /// Builds a [`SandboxError::Execution`] variant.
    pub fn execution(msg: impl Into<String>) -> Self {
        Self::Execution(msg.into())
    }

    /// Whether this is a fuel exhaustion error.
    #[must_use]
    pub const fn is_fuel_exhausted(&self) -> bool {
        matches!(self, Self::FuelExhausted { .. })
    }

    /// Whether this is an unimplemented-operation error.
    #[must_use]
    pub const fn is_not_implemented(&self) -> bool {
        matches!(self, Self::NotImplemented(_))
    }
}

impl From<SandboxError> for familyclaw_core::FamilyClawError {
    /// Converts a sandbox error into the platform's centralized error type.
    ///
    /// All sandbox errors map most naturally onto either
    /// [`familyclaw_core::FamilyClawError::Bus`] or
    /// [`familyclaw_core::FamilyClawError::InvalidInput`]: fuel and
    /// execution errors are runtime errors (Bus), while capability and
    /// implementation errors qualify as input/validity errors.
    fn from(err: SandboxError) -> Self {
        match err {
            SandboxError::Capability(_) | SandboxError::NotImplemented(_) => {
                familyclaw_core::FamilyClawError::invalid_input(err.to_string())
            }
            other => familyclaw_core::FamilyClawError::bus(other.to_string()),
        }
    }
}

/// The sandbox's result type: [`std::result::Result`] whose error is
/// [`SandboxError`].
pub type Result<T> = std::result::Result<T, SandboxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuel_exhausted_display_and_predicate() {
        let err = SandboxError::fuel_exhausted(100, 150);
        assert!(err.is_fuel_exhausted());
        assert_eq!(err.to_string(), "fuel exhausted: budget 100, required 150");
    }

    #[test]
    fn capability_constructor() {
        let err = SandboxError::capability("no network");
        assert!(matches!(err, SandboxError::Capability(_)));
        assert_eq!(err.to_string(), "capability violation: no network");
    }

    #[test]
    fn not_implemented_predicate() {
        let err = SandboxError::not_implemented("need wasmtime feature");
        assert!(err.is_not_implemented());
        assert!(err.to_string().starts_with("not implemented:"));
    }

    #[test]
    fn setup_and_execution_constructors() {
        assert_eq!(
            SandboxError::setup("bad module").to_string(),
            "sandbox setup failed: bad module"
        );
        assert_eq!(
            SandboxError::execution("trap").to_string(),
            "execution failed: trap"
        );
    }

    #[test]
    fn converts_to_core_invalid_input_for_capability() {
        let err: familyclaw_core::FamilyClawError = SandboxError::capability("denied").into();
        assert!(matches!(
            err,
            familyclaw_core::FamilyClawError::InvalidInput(_)
        ));
    }

    #[test]
    fn converts_to_core_invalid_input_for_not_implemented() {
        let err: familyclaw_core::FamilyClawError = SandboxError::not_implemented("x").into();
        assert!(matches!(
            err,
            familyclaw_core::FamilyClawError::InvalidInput(_)
        ));
    }

    #[test]
    fn converts_to_core_bus_for_runtime_errors() {
        let fuel: familyclaw_core::FamilyClawError = SandboxError::fuel_exhausted(1, 2).into();
        assert!(matches!(fuel, familyclaw_core::FamilyClawError::Bus(_)));

        let exec: familyclaw_core::FamilyClawError = SandboxError::execution("trap").into();
        assert!(matches!(exec, familyclaw_core::FamilyClawError::Bus(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<SandboxError>();
    }
}

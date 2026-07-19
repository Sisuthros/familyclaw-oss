//! Default implementation [`NoopSandbox`] which does not execute code.
//!
//! This is a safe default when the `wasmtime` feature is not compiled in:
//! no code runs, so there is no attack surface. An execution attempt
//! returns [`SandboxError::NotImplemented`] with a clear message directing
//! the caller to enable the `wasmtime` feature for real execution.
//!
//! `NoopSandbox` **still validates the request** (code not empty,
//! capabilities well-formed) before returning `NotImplemented`. This way
//! the caller's request errors are surfaced even without the wasmtime
//! backend.

use crate::error::SandboxError;
use crate::sandbox::{CodeSandbox, SandboxRequest, SandboxResult};

/// A sandbox implementation that does not execute any code.
///
/// Use when:
/// - the `wasmtime` feature is not enabled, or
/// - all code execution should be explicitly denied.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct NoopSandbox;

impl NoopSandbox {
    /// Creates a new [`NoopSandbox`] instance.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CodeSandbox for NoopSandbox {
    fn execute(&self, request: &SandboxRequest) -> SandboxResult {
        // Validate the request first: caller errors surface even without a backend.
        request.validate()?;
        Err(SandboxError::not_implemented(
            "NoopSandbox does not execute code; enable the `wasmtime` feature \
             for real sandboxed execution",
        ))
    }

    fn backend_name(&self) -> &'static str {
        "noop"
    }

    fn can_execute(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, CapabilitySet};
    use crate::fuel::FuelLimit;

    #[test]
    fn reports_noop_backend_and_cannot_execute() {
        let sandbox = NoopSandbox::new();
        assert_eq!(sandbox.backend_name(), "noop");
        assert!(!sandbox.can_execute());
    }

    #[test]
    fn execute_returns_not_implemented_for_valid_request() {
        let sandbox = NoopSandbox::new();
        let req = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
        let err = sandbox.execute(&req).expect_err("noop never executes");
        assert!(err.is_not_implemented());
        assert!(err.to_string().contains("wasmtime"));
    }

    #[test]
    fn execute_validates_before_reporting_not_implemented() {
        let sandbox = NoopSandbox::new();
        // Empty code -> a setup error, NOT NotImplemented.
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = sandbox.execute(&req).expect_err("empty code rejected");
        assert!(!err.is_not_implemented());
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn execute_rejects_bad_capabilities_before_not_implemented() {
        let sandbox = NoopSandbox::new();
        let bad = CapabilitySet::deny_all().with(Capability::env_var("  "));
        let req = SandboxRequest::new(vec![1]).with_capabilities(bad);
        let err = sandbox.execute(&req).expect_err("bad caps rejected");
        assert!(!err.is_not_implemented());
    }

    #[test]
    fn usable_as_trait_object() {
        let sandbox: Box<dyn CodeSandbox> = Box::new(NoopSandbox::default());
        let req = SandboxRequest::new(vec![1]).with_fuel_limit(FuelLimit::limited(10));
        assert!(sandbox.execute(&req).is_err());
        assert_eq!(sandbox.backend_name(), "noop");
    }
}

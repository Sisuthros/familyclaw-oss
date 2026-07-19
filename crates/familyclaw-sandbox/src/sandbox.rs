//! The sandbox interface: the [`CodeSandbox`] trait plus the execution's
//! input and output types.
//!
//! The interface is backend-independent: the default implementation
//! [`NoopSandbox`](crate::NoopSandbox) does not execute code (it returns
//! [`SandboxError::NotImplemented`](crate::SandboxError::NotImplemented)),
//! and the real wasmtime-based implementation lives behind the `wasmtime`
//! feature. This keeps the whole workspace build lightweight when the
//! sandbox isn't needed.

use serde::{Deserialize, Serialize};

use crate::capability::CapabilitySet;
use crate::fuel::FuelLimit;

/// A request for a single sandbox execution.
///
/// Gathers everything an execution needs: the WASM bytecode to run, the
/// fuel limit, and the granted capabilities. Typically built in builder
/// style; the defaults are safe (limited fuel, no capabilities).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRequest {
    /// The WASM module to execute, as bytecode (`.wasm`).
    pub code: Vec<u8>,

    /// The fuel limit for this execution.
    pub fuel_limit: FuelLimit,

    /// The capabilities granted to the execution (empty by default = "deny all").
    pub capabilities: CapabilitySet,
}

impl SandboxRequest {
    /// Builds a request from the given WASM bytecode with safe defaults
    /// (limited default fuel, no capabilities).
    #[must_use]
    pub fn new(code: impl Into<Vec<u8>>) -> Self {
        Self {
            code: code.into(),
            fuel_limit: FuelLimit::default(),
            capabilities: CapabilitySet::deny_all(),
        }
    }

    /// Sets the fuel limit (builder style).
    #[must_use]
    pub fn with_fuel_limit(mut self, limit: FuelLimit) -> Self {
        self.fuel_limit = limit;
        self
    }

    /// Sets the capability set (builder style).
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Validates the request before execution.
    ///
    /// Checks that code has been provided and that the capability set is
    /// well-formed. The fuel limit is always valid by virtue of its type.
    ///
    /// # Errors
    /// [`crate::SandboxError::Setup`] if the code is empty, or
    /// [`crate::SandboxError::Capability`] if some capability is invalid.
    pub fn validate(&self) -> crate::Result<()> {
        if self.code.is_empty() {
            return Err(crate::SandboxError::setup("sandbox code must not be empty"));
        }
        self.capabilities.validate()?;
        Ok(())
    }
}

/// The result of a successful sandbox execution.
///
/// Serializable so the result can be recorded to a durable log or passed
/// across the bus. `output` is the byte stream produced by the executed
/// code (e.g. stdout or an explicit return value); `fuel_consumed` measures
/// the cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxOutput {
    /// The output byte stream produced by the code.
    #[serde(default)]
    pub output: Vec<u8>,

    /// The fuel consumed by the execution.
    pub fuel_consumed: u64,
}

impl SandboxOutput {
    /// Builds a result from a byte stream and consumption.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>, fuel_consumed: u64) -> Self {
        Self {
            output: output.into(),
            fuel_consumed,
        }
    }

    /// Interprets the output bytes as a UTF-8 string (lossy).
    ///
    /// Invalid bytes are replaced with the U+FFFD character, so this never
    /// fails — suitable for logging and diagnostics.
    #[must_use]
    pub fn output_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

/// The overall result of a sandbox execution.
///
/// A type alias that clarifies [`CodeSandbox::execute`]'s signature.
pub type SandboxResult = crate::Result<SandboxOutput>;

/// The interface for isolated code execution.
///
/// Implementations run (or refuse to run) WASM bytecode under a given fuel
/// limit and capability set. Contract:
/// - **Determinism within limits:** the same [`SandboxRequest`] produces the
///   same result if the WASM itself is deterministic (a prerequisite for
///   durable replay).
/// - **Secure by default:** without a granted capability, code has no
///   network, file, or environment variable access.
/// - **Fuel enforces the limit:** an infinite loop is interrupted with
///   [`SandboxError::FuelExhausted`](crate::SandboxError::FuelExhausted).
///
/// `Send + Sync` so the sandbox can be shared between actors on the bus.
pub trait CodeSandbox: Send + Sync {
    /// Runs the given request and returns the result.
    ///
    /// # Errors
    /// - [`SandboxError::Setup`](crate::SandboxError::Setup) if the request
    ///   is invalid or the module fails to load.
    /// - [`SandboxError::Capability`](crate::SandboxError::Capability) for a
    ///   capability violation.
    /// - [`SandboxError::FuelExhausted`](crate::SandboxError::FuelExhausted)
    ///   if fuel runs out.
    /// - [`SandboxError::Execution`](crate::SandboxError::Execution) for
    ///   another execution error.
    /// - [`SandboxError::NotImplemented`](crate::SandboxError::NotImplemented)
    ///   if the backend does not support execution (e.g. the default
    ///   `NoopSandbox`).
    fn execute(&self, request: &SandboxRequest) -> SandboxResult;

    /// The backend identifier, for logging and diagnostics.
    ///
    /// The default implementation returns `"unknown"`; concrete backends
    /// override this (e.g. `"noop"`, `"wasmtime"`).
    fn backend_name(&self) -> &'static str {
        "unknown"
    }

    /// Whether this backend can actually execute code.
    ///
    /// `true` by default. [`NoopSandbox`](crate::NoopSandbox) returns
    /// `false`, so the caller can check the situation before executing.
    fn can_execute(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;

    #[test]
    fn request_defaults_are_safe() {
        let req = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
        assert_eq!(req.fuel_limit, FuelLimit::default());
        assert!(req.capabilities.is_empty());
        assert!(req.validate().is_ok());
    }

    #[test]
    fn request_builder_sets_fields() {
        let caps = CapabilitySet::deny_all().with(Capability::network("h"));
        let req = SandboxRequest::new(vec![1, 2, 3])
            .with_fuel_limit(FuelLimit::limited(42))
            .with_capabilities(caps.clone());
        assert_eq!(req.fuel_limit, FuelLimit::limited(42));
        assert_eq!(req.capabilities, caps);
        assert_eq!(req.code, vec![1, 2, 3]);
    }

    #[test]
    fn request_validate_rejects_empty_code() {
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = req.validate().expect_err("empty code must fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn request_validate_rejects_bad_capabilities() {
        let bad = CapabilitySet::deny_all().with(Capability::network("  "));
        let req = SandboxRequest::new(vec![1]).with_capabilities(bad);
        assert!(req.validate().is_err());
    }

    #[test]
    fn output_string_lossy_handles_invalid_utf8() {
        let out = SandboxOutput::new(vec![0xff, 0xfe], 10);
        // Does not panic, replaces invalid bytes.
        let s = out.output_string_lossy();
        assert!(!s.is_empty());
    }

    #[test]
    fn output_string_lossy_decodes_valid_utf8() {
        let out = SandboxOutput::new(b"hello".to_vec(), 5);
        assert_eq!(out.output_string_lossy(), "hello");
        assert_eq!(out.fuel_consumed, 5);
    }

    #[test]
    fn output_serde_roundtrip() {
        let out = SandboxOutput::new(b"data".to_vec(), 99);
        let json = serde_json::to_string(&out).expect("serialize");
        let back: SandboxOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(out, back);
    }

    // A small test double proving the trait is object-safe and that the
    // default methods work.
    struct EchoSandbox;
    impl CodeSandbox for EchoSandbox {
        fn execute(&self, request: &SandboxRequest) -> SandboxResult {
            request.validate()?;
            Ok(SandboxOutput::new(request.code.clone(), 1))
        }
    }

    #[test]
    fn trait_is_object_safe_and_defaults_apply() {
        let sandbox: Box<dyn CodeSandbox> = Box::new(EchoSandbox);
        assert_eq!(sandbox.backend_name(), "unknown");
        assert!(sandbox.can_execute());
        let req = SandboxRequest::new(vec![7, 8, 9]);
        let out = sandbox.execute(&req).expect("echo ok");
        assert_eq!(out.output, vec![7, 8, 9]);
        assert_eq!(out.fuel_consumed, 1);
    }
}

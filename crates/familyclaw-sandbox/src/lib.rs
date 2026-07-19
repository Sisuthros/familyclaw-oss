//! # familyclaw-sandbox
//!
//! Isolated code execution for the `FamilyClaw` platform: a WASM-based
//! sandbox where **fuel enforces an execution ceiling** and a
//! **capability model restricts access** (design §2 security). This is a
//! Layer A (OSS) crate — it does not hardcode family members' souls, keys,
//! or paths.
//!
//! ## Structure
//! The crate is deliberately layered so the security logic is testable
//! without the heavy wasmtime dependency:
//!
//! - [`CodeSandbox`] — a backend-independent interface
//!   ([`execute`](CodeSandbox::execute)).
//! - [`Capability`] / [`CapabilitySet`] — a "deny by default" capability
//!   model (network, files, environment variables).
//! - [`FuelLimit`] / [`FuelMeter`] — the fuel budget and its measurement.
//! - [`NoopSandbox`] — the **default implementation** that does not run
//!   code (returns [`SandboxError::NotImplemented`]). Safe when wasmtime
//!   isn't needed.
//! - `WasmtimeSandbox` — the real wasmtime-based implementation, **behind
//!   the `wasmtime` feature** (see below).
//!
//! ## Feature flags
//! - **`wasmtime`** (not default): enables the
//!   `WasmtimeSandbox` implementation. wasmtime is a large
//!   dependency (Cranelift + JIT), so it's optional to avoid slowing down
//!   the whole workspace build. Without this feature only [`NoopSandbox`]
//!   is available.
//!
//! ```toml
//! [dependencies]
//! familyclaw-sandbox = { version = "0.1", features = ["wasmtime"] }
//! ```
//!
//! ## Example (default, without wasmtime)
//! ```
//! use familyclaw_sandbox::{CodeSandbox, NoopSandbox, SandboxRequest};
//!
//! let sandbox = NoopSandbox::new();
//! assert!(!sandbox.can_execute());
//!
//! let request = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
//! // NoopSandbox validates the request but does not run the code.
//! let result = sandbox.execute(&request);
//! assert!(result.is_err());
//! ```
//!
//! ## Security principles
//! - **Deny by default:** without a granted [`Capability`], executed code
//!   has no network, file, or environment variable access.
//! - **Fuel enforces the limit:** an infinite loop is interrupted with
//!   [`SandboxError::FuelExhausted`].
//! - **Determinism:** the same [`SandboxRequest`] produces the same result
//!   (a prerequisite for durable replay).
//!
//! ## Containment requirements (2604.23425) — where the crate enforces each one
//!
//! The paper derives five architectural requirements for isolated code
//! execution from an analysis of 698 incidents. This crate maps onto them
//! as follows:
//!
//! 1. **Resource limits** — [`FuelLimit`] / [`FuelMeter`]. The fuel budget
//!    cuts off infinite loops and resource abuse; exceeding it returns
//!    [`SandboxError::FuelExhausted`].
//! 2. **Network isolation** — [`CapabilitySet`]
//!    ([`allows_network_host`](CapabilitySet::allows_network_host)).
//!    By default ([`deny_all`](CapabilitySet::deny_all)) there is no
//!    network access; access only to explicitly granted hosts.
//! 3. **Filesystem sandboxing** —
//!    [`CapabilitySet`]
//!    ([`allows_read_path`](CapabilitySet::allows_read_path)).
//!    Component-level prefix comparison restricts reads to granted
//!    subtrees; other path access is denied.
//! 4. **Capability access** — [`Capability`] /
//!    [`CapabilitySet`] as a whole: an additive "deny by default"
//!    model, where [`validate`](CapabilitySet::validate) rejects
//!    malformed grants.
//! 5. **Audit logging** — [`AuditLog`] /
//!    [`AuditedCapabilities`]. An append-only log records every
//!    capability check (granted/denied) as well as the start and end of
//!    executions. Wired in as an **optional** hook without changing the
//!    public interface of existing types.
//!
//! In addition, [`replay`](mod@replay) implements the LOOP mechanism
//! (2605.14237): execution is recorded as an [`ExecutionTrace`] and
//! replayed deterministically from the log alone, which enables
//! bit-exact post-hoc review of containment events without the original
//! backend.

pub mod audit;
pub mod capability;
pub mod error;
pub mod fuel;
pub mod noop;
pub mod replay;
pub mod sandbox;

#[cfg(feature = "wasmtime")]
pub mod wasmtime_backend;

pub use audit::{AuditEntry, AuditLog, AuditedCapabilities, CapabilityCheck};
pub use capability::{Capability, CapabilitySet};
pub use error::{Result, SandboxError};
pub use fuel::{FuelLimit, FuelMeter};
pub use noop::NoopSandbox;
pub use replay::{replay, ExecutionTrace, Outcome, TraceEvent};
pub use sandbox::{CodeSandbox, SandboxOutput, SandboxRequest, SandboxResult};

#[cfg(feature = "wasmtime")]
pub use wasmtime_backend::WasmtimeSandbox;

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Reports whether the real wasmtime-based sandbox was compiled in.
///
/// `true` when the `wasmtime` feature is active (host-import denial + fuel
/// cap enforced, [`default_sandbox`] returns a `WasmtimeSandbox`); `false`
/// when only [`NoopSandbox`] is available. This is a compile-time constant,
/// so a caller (e.g. the gateway's `doctor`/`status`) can report sandbox
/// availability without depending on the `cfg!` macro in its own crate.
#[must_use]
pub const fn wasmtime_available() -> bool {
    cfg!(feature = "wasmtime")
}

/// Returns a human-readable sandbox availability label.
///
/// `wasmtime (host-import denial + fuel cap)` when [`wasmtime_available`] is
/// `true`, otherwise `none (noop)`. Intended for operator-facing status
/// output (gateway `doctor`/`status`); deterministic and secret-free.
#[must_use]
pub const fn sandbox_availability() -> &'static str {
    if wasmtime_available() {
        "wasmtime (host-import denial + fuel cap)"
    } else {
        "none (noop)"
    }
}

/// Returns the default sandbox boxed as a trait object.
///
/// With the `wasmtime` feature this is a
/// `WasmtimeSandbox`; without it,
/// [`NoopSandbox`]. This gives the caller a backend-independent way to get
/// the "best available" sandbox.
///
/// # Errors
/// [`SandboxError::Setup`] if the `wasmtime` backend fails to initialize.
/// The noop case never fails.
pub fn default_sandbox() -> Result<Box<dyn CodeSandbox>> {
    #[cfg(feature = "wasmtime")]
    {
        Ok(Box::new(wasmtime_backend::WasmtimeSandbox::new()?))
    }
    #[cfg(not(feature = "wasmtime"))]
    {
        Ok(Box::new(NoopSandbox::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn sandbox_availability_matches_compiled_feature() {
        // The availability label + flag follow the compile-time wasmtime feature.
        if cfg!(feature = "wasmtime") {
            assert!(wasmtime_available());
            assert_eq!(
                sandbox_availability(),
                "wasmtime (host-import denial + fuel cap)"
            );
        } else {
            assert!(!wasmtime_available());
            assert_eq!(sandbox_availability(), "none (noop)");
        }
    }

    #[test]
    fn public_api_is_reexported() {
        // Confirms the public surface is available from the crate root.
        let _cap: Capability = Capability::network("h");
        let _caps: CapabilitySet = CapabilitySet::deny_all();
        let _limit: FuelLimit = FuelLimit::default();
        let _meter: FuelMeter = FuelMeter::default();
        let _req: SandboxRequest = SandboxRequest::new(vec![1]);
        let _out: SandboxOutput = SandboxOutput::new(vec![], 0);
        let _err: SandboxError = SandboxError::capability("x");
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());

        let sandbox: NoopSandbox = NoopSandbox::new();
        let _name = sandbox.backend_name();
    }

    #[test]
    fn default_sandbox_is_constructible_and_usable() {
        let sandbox = default_sandbox().expect("default sandbox builds");
        // The request is validated regardless of the backend.
        let bad = SandboxRequest::new(Vec::<u8>::new());
        assert!(sandbox.execute(&bad).is_err());
    }

    #[cfg(not(feature = "wasmtime"))]
    #[test]
    fn default_sandbox_is_noop_without_feature() {
        let sandbox = default_sandbox().expect("noop builds");
        assert_eq!(sandbox.backend_name(), "noop");
        assert!(!sandbox.can_execute());
    }

    #[cfg(feature = "wasmtime")]
    #[test]
    fn default_sandbox_is_wasmtime_with_feature() {
        let sandbox = default_sandbox().expect("wasmtime builds");
        assert_eq!(sandbox.backend_name(), "wasmtime");
        assert!(sandbox.can_execute());
    }
}

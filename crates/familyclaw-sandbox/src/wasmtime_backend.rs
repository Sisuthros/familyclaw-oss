//! Real wasmtime-based sandbox implementation.
//!
//! **Compiled only with the `wasmtime` feature.** This module wires the
//! [`CodeSandbox`] interface to the wasmtime runtime: fuel enforces the
//! execution ceiling, and the capability model restricts access. wasmtime is
//! a large dependency (Cranelift + JIT), so it is optional so it does not
//! slow down the workspace build when the sandbox is not needed.
//!
//! ## Execution convention
//! The WASM module to be executed must export a parameterless function
//! named [`WasmtimeSandbox::ENTRY_POINT`] that returns an `i32` status code.
//! The module is run without host imports: since capabilities (network, FS)
//! are not enabled by default, a module that requires imports is rejected
//! with a clear [`SandboxError::Setup`] error. This is a deliberate security
//! boundary — extended WASI capabilities will be added later, governed by
//! the capability model.

use wasmtime::{Config, Engine, Instance, Module, Store, Trap};

use crate::error::SandboxError;
use crate::sandbox::{CodeSandbox, SandboxOutput, SandboxRequest, SandboxResult};

/// wasmtime-based [`CodeSandbox`] implementation.
///
/// A single instance encapsulates a shared [`Engine`] (which holds the
/// compiled-code cache) and is `Send + Sync`, so it can be shared across
/// bus actors.
#[derive(Clone)]
pub struct WasmtimeSandbox {
    engine: Engine,
}

impl std::fmt::Debug for WasmtimeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `wasmtime::Engine` does not implement Debug, so only the type is shown.
        f.debug_struct("WasmtimeSandbox").finish_non_exhaustive()
    }
}

impl WasmtimeSandbox {
    /// The name of the required exported function (no parameters, returns `i32`).
    pub const ENTRY_POINT: &'static str = "run";

    /// Creates a new wasmtime sandbox with fuel metering enabled.
    ///
    /// # Errors
    /// [`SandboxError::Setup`] if the wasmtime engine cannot be initialized
    /// with the given configuration.
    pub fn new() -> crate::Result<Self> {
        let mut config = Config::new();
        // Fuel metering enforces the execution ceiling — a core security feature.
        config.consume_fuel(true);
        // Native unwind info: needed so that trap unwinding (e.g. fuel
        // exhaustion) works correctly instead of triggering a
        // __fastfail abort on Windows.
        config.native_unwind_info(true);
        // No guest backtraces: the sandbox does not expose a stack trace of
        // untrusted code, and this reduces overhead. `wasm_backtrace(false)`
        // is deprecated in newer wasmtime — `None` removes the backtrace
        // context entirely, which matches the old behavior exactly.
        config.wasm_backtrace_max_frames(None);
        let engine =
            Engine::new(&config).map_err(|e| SandboxError::setup(format!("engine init: {e}")))?;
        Ok(Self { engine })
    }

    /// Access to the shared [`Engine`] (e.g. for precompiling modules).
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl CodeSandbox for WasmtimeSandbox {
    fn execute(&self, request: &SandboxRequest) -> SandboxResult {
        // 1) Validate the request (code not empty, capabilities well-formed).
        request.validate()?;

        // 2) Compile the module from the given WASM bytecode. `Module::new`
        //    is safe (unlike `deserialize`), so the unsafe-code prohibition
        //    is not violated.
        let module = Module::new(&self.engine, &request.code)
            .map_err(|e| SandboxError::setup(format!("module compile: {e}")))?;

        // 3) Security boundary: a module requiring imports is not run.
        //    Without granted capabilities, the host provides nothing, so the
        //    import would be left unlinked. Reject with a clear message.
        if module.imports().len() > 0 {
            return Err(SandboxError::setup(
                "module requires host imports, which are not granted by the current \
                 capability set",
            ));
        }

        // 4) Create the store and set the fuel budget. "Unlimited" means
        //    u64::MAX in practice (consume_fuel is still enabled per the
        //    engine's requirement, but the ceiling is effectively infinite).
        let mut store = Store::new(&self.engine, ());
        let budget = request.fuel_limit.budget().unwrap_or(u64::MAX);
        store
            .set_fuel(budget)
            .map_err(|e| SandboxError::setup(format!("set fuel: {e}")))?;

        // 5) Instantiate the module without imports.
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|e| SandboxError::setup(format!("instantiate: {e}")))?;

        // 6) Look up the agreed entry point and call it.
        let entry = instance
            .get_typed_func::<(), i32>(&mut store, Self::ENTRY_POINT)
            .map_err(|e| {
                SandboxError::setup(format!("missing entry point `{}`: {e}", Self::ENTRY_POINT))
            })?;

        let fuel_before = store
            .get_fuel()
            .map_err(|e| SandboxError::execution(format!("read fuel: {e}")))?;

        let status = match entry.call(&mut store, ()) {
            Ok(status) => status,
            Err(err) => {
                // Distinguish fuel exhaustion from other traps.
                if err.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
                    // `required` is at least budget+1 (saturating: does not
                    // panic even if budget were u64::MAX in the unlimited
                    // case, which in practice never happens when fuel runs out).
                    return Err(SandboxError::fuel_exhausted(
                        budget,
                        budget.saturating_add(1),
                    ));
                }
                return Err(SandboxError::execution(format!("trap: {err}")));
            }
        };

        // 7) Compute the fuel consumed.
        let fuel_after = store
            .get_fuel()
            .map_err(|e| SandboxError::execution(format!("read fuel: {e}")))?;
        let fuel_consumed = fuel_before.saturating_sub(fuel_after);

        // 8) Pack the status code into little-endian bytes for the result. A
        //    broader memory-based output convention will be added along with
        //    the capability model.
        Ok(SandboxOutput::new(
            status.to_le_bytes().to_vec(),
            fuel_consumed,
        ))
    }

    fn backend_name(&self) -> &'static str {
        "wasmtime"
    }

    fn can_execute(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuel::FuelLimit;

    /// A small WAT module that exports the `run` function and returns the given value.
    fn wat_returning(value: i32) -> Vec<u8> {
        let wat = format!(r#"(module (func (export "run") (result i32) (i32.const {value})))"#);
        wat::parse_str(&wat).expect("valid wat compiles to wasm")
    }

    #[test]
    fn backend_metadata() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        assert_eq!(sandbox.backend_name(), "wasmtime");
        assert!(sandbox.can_execute());
    }

    #[test]
    fn executes_simple_module_and_returns_status() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(wat_returning(7));
        let out = sandbox.execute(&req).expect("simple module runs");
        assert_eq!(out.output, 7_i32.to_le_bytes().to_vec());
        // Some fuel is consumed.
        assert!(out.fuel_consumed > 0);
    }

    #[test]
    fn rejects_empty_code() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = sandbox.execute(&req).expect_err("empty rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn rejects_invalid_wasm() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(vec![0xde, 0xad, 0xbe, 0xef]);
        let err = sandbox.execute(&req).expect_err("garbage rejected");
        assert!(err.to_string().contains("module compile"));
    }

    #[test]
    fn rejects_missing_entry_point() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let wat = r#"(module (func (export "other") (result i32) (i32.const 1)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm);
        let err = sandbox.execute(&req).expect_err("missing run rejected");
        assert!(err.to_string().contains("entry point"));
    }

    #[test]
    fn rejects_module_with_imports() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let wat = r#"(module
            (import "host" "f" (func))
            (func (export "run") (result i32) (i32.const 0)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm);
        let err = sandbox.execute(&req).expect_err("imports rejected");
        assert!(err.to_string().contains("host imports"));
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        // Infinite loop — must be interrupted by fuel exhaustion.
        let wat = r#"(module (func (export "run") (result i32)
            (loop (br 0)) (i32.const 0)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm).with_fuel_limit(FuelLimit::limited(10_000));
        let err = sandbox.execute(&req).expect_err("infinite loop traps");
        assert!(err.is_fuel_exhausted());
    }

    #[test]
    fn fuel_consumed_scales_with_work() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        // Little work vs. more work -> more fuel consumed.
        let light = r#"(module (func (export "run") (result i32) (i32.const 0)))"#;
        let heavy = r#"(module (func (export "run") (result i32)
            (local $i i32)
            (loop $l
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_s (local.get $i) (i32.const 1000))))
            (local.get $i)))"#;
        let light_out = sandbox
            .execute(&SandboxRequest::new(wat::parse_str(light).expect("wat")))
            .expect("light runs");
        let heavy_out = sandbox
            .execute(&SandboxRequest::new(wat::parse_str(heavy).expect("wat")))
            .expect("heavy runs");
        assert!(heavy_out.fuel_consumed > light_out.fuel_consumed);
    }

    #[test]
    fn deterministic_fuel_for_same_input() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let code = wat_returning(42);
        let a = sandbox
            .execute(&SandboxRequest::new(code.clone()))
            .expect("run a");
        let b = sandbox.execute(&SandboxRequest::new(code)).expect("run b");
        // Determinism: same input -> same consumption + same result (durable replay).
        assert_eq!(a.fuel_consumed, b.fuel_consumed);
        assert_eq!(a.output, b.output);
    }
}

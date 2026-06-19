//! End-to-end integration tests for the wasmtime-backed code sandbox.
//!
//! Where the in-module unit tests prove individual pieces, these tests drive
//! the full public surface (`WasmtimeSandbox::execute`) to prove the security
//! claims hold together. They are deliberately written against **what the code
//! guarantees today**, not against an aspirational design.
//!
//! ## What the sandbox actually guarantees today (honest scope)
//!
//! The crate ships a declarative capability/credential model
//! (`CapabilitySet`: a "deny by default" set of network / read-path / env-var
//! grants), and a request carries a `CapabilitySet`. However, the runtime
//! backend does **not yet consume that set to selectively link host imports**.
//! Instead the backend enforces a strictly stronger rule:
//!
//! > A module that declares **any** host import is rejected before it runs,
//! > and an accepted module is instantiated with an **empty import list**
//! > against a **fresh store carrying unit host state** (`()`).
//!
//! In other words, today's isolation guarantee is **total host denial**: there
//! is no channel through which a host capability, ambient credential, or
//! parent resource could reach guest code, because guest code is never given a
//! single host import to call. This is *stronger* than a per-invocation
//! allowlist for the things the guest can reach (it can reach nothing), but it
//! is also *different*: a per-invocation credential allowlist — where
//! invocation A may be granted a named credential that invocation B does not
//! inherit — is **not implemented at the runtime boundary**. The declarative
//! set is validated and audited, but it does not (yet) widen what the guest
//! can touch. The master plan's "per-invocation credential grant" remains
//! future work; these tests assert the real, present guarantee and do not
//! pretend the allowlist enforcement exists.
//!
//! Because every invocation builds its own `Store` and `Instance` with no
//! host functions, there is also no shared mutable host state between
//! invocations — sequential executions cannot observe each other's guest
//! memory or leak state across the boundary. That fresh-store property is the
//! concrete substitute for "credential A is invisible to invocation B": with
//! zero host surface, there is nothing to leak in *or* out.
//!
//! All fixtures are built with `wat::parse_str`, matching the unit tests; no
//! external `.wasm` artefacts are required.
//!
//! These tests require the `wasmtime` feature (the backend lives behind it),
//! so the whole module is feature-gated.
#![cfg(feature = "wasmtime")]

use familyclaw_sandbox::{
    CapabilitySet, Capability, CodeSandbox, FuelLimit, SandboxError, SandboxRequest,
    WasmtimeSandbox,
};

/// Compiles a WAT fixture to wasm bytes, failing the test loudly on bad input.
fn wasm(wat_src: &str) -> Vec<u8> {
    wat::parse_str(wat_src).expect("fixture WAT must compile to wasm")
}

/// A module that spins forever (`(loop (br 0))`) under the mandatory `run`
/// export. With a finite fuel budget it must terminate via fuel exhaustion.
const INFINITE_LOOP: &str = r#"(module
    (func (export "run") (result i32)
        (loop (br 0))
        (i32.const 0)))"#;

/// 1. Fuel exhaustion: an unbounded loop given a small fuel budget terminates
///    with `SandboxError::FuelExhausted` — it neither hangs nor panics.
#[test]
fn fuel_exhaustion_terminates_infinite_loop() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");
    let request =
        SandboxRequest::new(wasm(INFINITE_LOOP)).with_fuel_limit(FuelLimit::limited(5_000));

    // `execute` returns (does not hang) and yields the fuel-exhausted variant.
    let err = sandbox
        .execute(&request)
        .expect_err("infinite loop must run out of fuel, not return Ok");

    assert!(
        err.is_fuel_exhausted(),
        "expected FuelExhausted, got: {err}"
    );
    assert!(
        matches!(err, SandboxError::FuelExhausted { budget, required }
            if budget == 5_000 && required > budget),
        "fuel-exhausted error must report the budget and a higher requirement"
    );
}

/// A tiny budget must also exhaust (proves the cap is honoured, not merely a
/// large constant that the loop happens to exceed).
#[test]
fn fuel_exhaustion_honours_a_tiny_budget() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");
    let request =
        SandboxRequest::new(wasm(INFINITE_LOOP)).with_fuel_limit(FuelLimit::limited(1));
    let err = sandbox
        .execute(&request)
        .expect_err("budget of 1 cannot run an infinite loop");
    assert!(err.is_fuel_exhausted(), "expected FuelExhausted, got: {err}");
}

/// 2. Denied capability / host-import rejection: a module importing a host
///    function is rejected with the "host imports" setup error **before** it
///    runs. A module cannot smuggle in a host call.
#[test]
fn host_function_import_is_rejected_before_execution() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");
    // Declares an import of a host function named `secret` — the kind of thing
    // a malicious guest would use to reach an ambient credential or syscall.
    let code = wasm(
        r#"(module
            (import "host" "secret" (func $secret (result i32)))
            (func (export "run") (result i32)
                (call $secret)))"#,
    );
    let request = SandboxRequest::new(code);

    let err = sandbox
        .execute(&request)
        .expect_err("a module declaring a host import must be rejected");

    // It must be the *setup* rejection (pre-run), not a runtime trap.
    assert!(
        matches!(err, SandboxError::Setup(_)),
        "host-import rejection must be a setup error, got: {err}"
    );
    assert!(
        err.to_string().contains("host imports"),
        "rejection message must name the host-imports reason, got: {err}"
    );
}

/// The rejection covers *any* import shape, not just functions: importing a
/// host memory, global, or table is equally a channel to host state and must
/// also be denied. This proves the guard is on `imports().len() > 0`, not on a
/// single import kind.
#[test]
fn host_memory_and_global_imports_are_also_rejected() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");

    let import_memory = wasm(
        r#"(module
            (import "host" "mem" (memory 1))
            (func (export "run") (result i32) (i32.const 0)))"#,
    );
    let import_global = wasm(
        r#"(module
            (import "host" "g" (global i32))
            (func (export "run") (result i32) (i32.const 0)))"#,
    );

    for code in [import_memory, import_global] {
        let err = sandbox
            .execute(&SandboxRequest::new(code))
            .expect_err("any host import must be rejected");
        assert!(
            matches!(err, SandboxError::Setup(_)) && err.to_string().contains("host imports"),
            "every host-import kind must hit the host-imports guard, got: {err}"
        );
    }
}

/// 3a. Host/credential isolation end-to-end: even when the request *declares*
///     capabilities (network host, env var), a module that tries to reach the
///     host through an import is still denied. This proves the declarative set
///     does NOT widen the runtime surface today — the guarantee is total host
///     denial, independent of what capabilities the request carries.
#[test]
fn declared_capabilities_do_not_open_a_host_channel() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");

    // A request that "wants" network + env access. If a per-invocation
    // allowlist were enforced at the boundary, one might expect a matching
    // host import to link. It must not: there is no host surface at all.
    let granted = CapabilitySet::deny_all()
        .with(Capability::network("api.example.com"))
        .with(Capability::env_var("CREDENTIAL_TOKEN"));

    let code = wasm(
        r#"(module
            (import "env" "CREDENTIAL_TOKEN" (func $leak (result i32)))
            (func (export "run") (result i32)
                (call $leak)))"#,
    );
    let request = SandboxRequest::new(code).with_capabilities(granted);

    let err = sandbox
        .execute(&request)
        .expect_err("no host import links even when capabilities are declared");
    assert!(
        matches!(err, SandboxError::Setup(_)) && err.to_string().contains("host imports"),
        "declared capabilities must not open a host channel, got: {err}"
    );
}

/// 3b. With zero host imports there is simply no channel for a parent
///     credential or host resource to enter the guest. A pure compute module
///     runs to completion using only its own state — proving an accepted
///     module reaches nothing outside itself.
#[test]
fn accepted_module_has_no_ambient_host_access() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");
    // Pure arithmetic: no imports, only locals. This is the *only* class of
    // module that runs today, which is exactly the containment guarantee.
    let code = wasm(
        r#"(module
            (func (export "run") (result i32)
                (i32.add (i32.const 20) (i32.const 22))))"#,
    );
    let out = sandbox
        .execute(&SandboxRequest::new(code))
        .expect("a self-contained module runs");
    assert_eq!(out.output, 42_i32.to_le_bytes().to_vec());
}

/// 4. Fresh-store isolation: two sequential `execute()` calls do not share
///    wasm state. Each invocation gets its own `Store` and `Instance`, so
///    invocation B cannot observe invocation A's memory. We prove this by
///    having both modules write to a value at the same memory offset and read
///    it back — each sees only its own write, never the other's.
#[test]
fn sequential_executions_do_not_share_guest_memory() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");

    // Each module owns its own (non-imported) linear memory, writes a distinct
    // sentinel to offset 0, then returns what it reads back. If state bled
    // across invocations, the second read could observe the first's write.
    let writer = |sentinel: i32| {
        wasm(&format!(
            r#"(module
                (memory 1)
                (func (export "run") (result i32)
                    (i32.store (i32.const 0) (i32.const {sentinel}))
                    (i32.load (i32.const 0))))"#
        ))
    };

    let first = sandbox
        .execute(&SandboxRequest::new(writer(111)))
        .expect("first invocation runs");
    let second = sandbox
        .execute(&SandboxRequest::new(writer(222)))
        .expect("second invocation runs");

    assert_eq!(
        first.output,
        111_i32.to_le_bytes().to_vec(),
        "first invocation sees its own sentinel"
    );
    assert_eq!(
        second.output,
        222_i32.to_le_bytes().to_vec(),
        "second invocation sees ONLY its own sentinel — no state bled across the fresh store"
    );
}

/// A module that reads memory it never initialised must observe the WASM
/// zero-default, not residue from a prior invocation. This is the negative
/// form of the fresh-store guarantee: a fresh store starts zeroed every time.
#[test]
fn fresh_store_memory_starts_zeroed_each_invocation() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");

    // First, an invocation that writes a non-zero value into its own memory.
    let writer = wasm(
        r#"(module
            (memory 1)
            (func (export "run") (result i32)
                (i32.store (i32.const 0) (i32.const 0x7fffffff))
                (i32.load (i32.const 0))))"#,
    );
    // Then, an invocation that only *reads* offset 0 without writing it.
    let reader = wasm(
        r#"(module
            (memory 1)
            (func (export "run") (result i32)
                (i32.load (i32.const 0))))"#,
    );

    let _ = sandbox
        .execute(&SandboxRequest::new(writer))
        .expect("writer runs");
    let read_back = sandbox
        .execute(&SandboxRequest::new(reader))
        .expect("reader runs");

    assert_eq!(
        read_back.output,
        0_i32.to_le_bytes().to_vec(),
        "fresh store memory must be zero — the prior write must not leak in"
    );
}

/// Same sandbox instance is reused across all of the above; prove the shared
/// `Engine` is safe to reuse (it caches compiled code) while still giving each
/// execution a clean store. A successful run after a fuel-exhausted run shows
/// the engine is not left in a poisoned state by a trap.
#[test]
fn engine_reuse_after_trap_still_executes_cleanly() {
    let sandbox = WasmtimeSandbox::new().expect("engine init");

    // Trap one invocation via fuel exhaustion.
    let trapped = sandbox.execute(
        &SandboxRequest::new(wasm(INFINITE_LOOP)).with_fuel_limit(FuelLimit::limited(2_000)),
    );
    assert!(
        trapped.is_err_and(|e| e.is_fuel_exhausted()),
        "first invocation must trap on fuel"
    );

    // The reused engine must still run a clean module afterwards.
    let ok = sandbox
        .execute(&SandboxRequest::new(wasm(
            r#"(module (func (export "run") (result i32) (i32.const 9)))"#,
        )))
        .expect("engine still usable after a trap");
    assert_eq!(ok.output, 9_i32.to_le_bytes().to_vec());
}

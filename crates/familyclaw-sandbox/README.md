# familyclaw-sandbox

Isolated code execution for the FamilyClaw platform (Layer A / OSS).

A WASM-based sandbox where **fuel enforces an execution ceiling** and a
**capability model restricts access** to the network, files, and
environment variables. Design reference: design §2 (security).

## Principle: secure by default

- **Deny by default** — without an explicitly granted `Capability`,
  executed code has no network, file, or environment variable access.
- **Fuel enforces the limit** — an infinite loop is interrupted with
  `SandboxError::FuelExhausted` instead of hanging.
- **Determinism** — the same `SandboxRequest` produces the same result (a
  prerequisite for durable replay).

## Structure

The crate is layered so the security logic is testable **without** the
heavy wasmtime dependency:

| Type | Responsibility |
|--------|--------|
| `CodeSandbox` (trait) | Backend-independent interface: `execute(&request) -> SandboxResult` |
| `Capability` / `CapabilitySet` | "Deny by default" capability model (network, FS reads, env) |
| `FuelLimit` / `FuelMeter` | Fuel budget and consumption measurement |
| `SandboxRequest` / `SandboxOutput` | Execution input and result (serde-serializable) |
| `NoopSandbox` | **Default implementation** — does not run code, returns `NotImplemented` |
| `WasmtimeSandbox` | Real wasmtime implementation, **behind the `wasmtime` feature** |

## Feature flags

| Feature | Default | Effect |
|---------|--------|----------|
| `wasmtime` | no | Enables the `WasmtimeSandbox` implementation. wasmtime is a large dependency (Cranelift + JIT), so it's optional to avoid slowing down the whole workspace build. |

Without the `wasmtime` feature only `NoopSandbox` is available, and
`default_sandbox()` returns it.

```toml
[dependencies]
familyclaw-sandbox = { version = "0.1", features = ["wasmtime"] }
```

## Usage

```rust
use familyclaw_sandbox::{CodeSandbox, SandboxRequest, FuelLimit, Capability, CapabilitySet};

// The "best available" backend (wasmtime if compiled in, otherwise noop).
let sandbox = familyclaw_sandbox::default_sandbox()?;

let request = SandboxRequest::new(wasm_bytes)
    .with_fuel_limit(FuelLimit::limited(1_000_000))
    .with_capabilities(
        CapabilitySet::deny_all().with(Capability::read_only_fs("/data")),
    );

let output = sandbox.execute(&request)?;
println!("fuel consumed: {}", output.fuel_consumed);
# Ok::<(), familyclaw_sandbox::SandboxError>(())
```

### wasmtime backend execution convention

`WasmtimeSandbox` runs a WASM module that exports a parameterless function
named `run` that returns an `i32` status code. The module runs **without
host imports** — a module that requires imports is rejected with a clear
error, since capabilities do not provide a host interface by default.
Broader WASI capabilities will be added later, governed by the capability
model.

## OSS boundary (Layer A)

This crate does not hardcode family members' souls, API keys, tokens, IP
addresses, or personal paths. It is a generic platform component.

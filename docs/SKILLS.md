# Skills & WASM Sandbox Authoring Guide

FamilyClaw agents act on the world through **skills** — the unit of "a thing an
agent can do" beyond generating text. This guide covers what a skill is, how
it's described and gated, how the WASM sandbox isolates untrusted code today,
and how to register and test your own skill. Everything here is verified
against the current source in `crates/familyclaw-actions` and
`crates/familyclaw-sandbox`, plus [`SECURITY_MODEL.md`](SECURITY_MODEL.md).

## What a skill is

A skill is a manifest + an execution body that the Action/Skill Runtime
(`familyclaw-actions`) drives through one fixed pipeline:

```
observe → plan → request approval (if needed) → execute → verify
        → persist proof → remember → report
```

The manifest ([`SkillManifest`], `crates/familyclaw-actions/src/manifest.rs`)
is pure data — no executable logic — and declares:

- `id`, `name`, `version`, `description`
- `permissions: Vec<SkillPermission>` — the capabilities the skill needs
- `risk: ActionRisk` — the risk class (see below)
- `approval_policy: ApprovalPolicy` — when human approval is required
- `input_hint` / `output_hint` — free-form human-readable hints
- `input_schema` — a JSON Schema advertised to an LLM as the tool's
  `parameters` (root must be a JSON object; enforced by `validate()`)
- `publisher` / `signature` — set only for **external** (third-party) skills

The execution body implements the `ActionExecutor` trait
(`crates/familyclaw-actions/src/executor.rs`) — an async function that takes
an `ActionRequest` (skill id, task id, JSON payload, an injected timestamp,
and an `input_untrusted` flag) and returns an `ActionResult`.

FamilyClaw ships two **genuinely functional** reference skills that exercise
the full pipeline against real integration surfaces:

| Skill | What it does |
|---|---|
| `fs_read` (`crates/familyclaw-actions/src/skills/fs_read.rs`) | Reads a local file through a canonicalized allowlist: resolves `..`, follows symlinks to their real target, and rejects any path that escapes the allowlisted root. The proof records only a SHA-256 path hash + file size + short summary — never the full file body. |
| `web_fetch` (`crates/familyclaw-actions/src/skills/web_fetch.rs`) | A real read-only HTTP GET with structural SSRF guards (rejects non-`http(s)` schemes, `localhost`, private/loopback/link-local/CGNAT ranges including the cloud metadata address) and no redirect following. |

`fs_read` denials are diagnosable without leaking any path: the agent-visible
error distinguishes an empty allowlist, a path that doesn't exist/canonicalize,
and a path that resolves fine but is outside every allowlisted root (the
latter states the configured root **count**, e.g. "outside all 3 allowlisted
root(s)"). When the target is a directory, the full entry-name listing (up to
64 names, with a "… and N more" tail) is returned in the tool-result `content`
field by default — the 120-byte proof `summary` stays a short, truncated copy.

The remaining bundled skills (`email_triage`, `github_issue_draft`,
`file_patch`, `discord_thread_summary`) are **example patterns**: complete,
tested implementations of the skill *contract* using deterministic in-memory
logic and generic placeholder data (`user@example.com`,
`example-org/example-repo`) — not disabled stubs, and not live integrations.
They exist to show the shape of the contract so you can swap in your own
provider call behind the same manifest and pipeline wiring.

## Risk classes and approval policy

`ActionRisk` (`crates/familyclaw-actions/src/policy.rs`) is the risk
classification a skill declares:

| Risk | Meaning |
|---|---|
| `ReadOnly` | No side effects. |
| `WriteLocal` | Local, reversible write. |
| `WriteExternal` | Writes to a third-party system. |
| `Irreversible` | Cannot be undone. |
| `SpendMoney` | A monetary transaction. |
| `SendMessage` | Sends a message visible outside the process. |
| `ExecuteCode` | Executes code. |

`ApprovalPolicy` is one of `AutoIfReadOnly`, `RequireApproval`, or
`AlwaysRequireApproval`. The actual decision is computed by
`required_approval(risk, policy)`, which is **fail-safe by construction**:

- `SpendMoney` and `Irreversible` **always** require approval, regardless of
  policy — a skill cannot opt out of approval for these by requesting
  auto-run.
- `ExecuteCode` and `SendMessage` also always require approval.
- Only `ReadOnly` and `WriteLocal` can ever auto-run, and only under a policy
  that permits it (`WriteLocal` auto-runs only under `RequireApproval`, never
  under `AutoIfReadOnly`).

`SkillManifest::validate()` additionally cross-checks **permission vs. risk
class** so a manifest can't mislabel itself into skipping approval: a skill
declaring `SkillPermission::SpendMoney` must carry `risk = SpendMoney`, and any
skill declaring `WriteExternal` or `SpendMoney` cannot be placed in an
auto-runnable risk class (`ReadOnly` / `WriteLocal`). A `WriteExternal`
permission also requires an `approval_policy` that can actually demand
approval (`RequireApproval` or `AlwaysRequireApproval` — not
`AutoIfReadOnly`).

**Policy is always derived from the manifest, never from the task payload.**
This means a prompt-injection attack embedded in a tool result cannot change a
skill's risk class or bypass its approval requirement — the payload is data,
not policy.

Manifest validation also runs a heuristic secret scanner
(`detect_secret_like`) over every text field and every string node in
`input_schema`, rejecting manifests that look like they embed an API key,
AWS access key, bearer token, or long high-entropy hex/base64 run.

## External (third-party) skills: signing

A manifest is **external** when `publisher` is set to a non-empty value.
External skills must carry an Ed25519 `signature` over the manifest's signing
payload (the manifest JSON with `signature` cleared), verified against a
trusted public key loaded from the path in `FAMILYCLAW_SKILL_REGISTRY` (a JSON
map of `publisher → hex-encoded Ed25519 public key`). Missing or invalid
signatures fail closed — the skill never enters the registry
(`SkillManifest::verify_external_signature`). Built-in Layer A skills leave
`publisher`/`signature` unset and skip this check entirely.

## Capability model (WASM sandbox)

`familyclaw-sandbox` provides `CodeSandbox`, a backend-independent trait for
running untrusted code:

- `CapabilitySet` (`crates/familyclaw-sandbox/src/capability.rs`) is a
  **deny-by-default** set of grants: `ReadOnlyFs { path }` (component-level
  path-prefix match, not string-prefix — `/data` does not grant `/data2`),
  `Network { host }`, and `EnvVar { name }`. An empty set denies everything.
  Capabilities are additive and validated for well-formedness (no blank
  host/path/name).
- `NoopSandbox` is the default implementation when the `wasmtime` feature is
  off — it does not execute code and returns `NotImplemented`.
- `WasmtimeSandbox` (feature `wasmtime`) is the real backend.

**Important current limitation:** the shipped `WasmtimeSandbox` backend does
**not yet** consult `CapabilitySet` to selectively link host functions. Instead
it enforces a strictly stronger rule: **any WASM module that declares one or
more host imports is rejected outright** at instantiation
(`wasmtime_backend.rs`, `module.imports().len() > 0` →
`SandboxError::Setup`), independent of what the request's `CapabilitySet`
grants. Every accepted module runs with an empty import list against a fresh
`Store<()>` — there is no channel through which a host capability, ambient
credential, or parent-process resource can reach guest code.

Practical consequence: **today's sandbox is for pure-compute WASM skills
only.** A module needs no host imports, exports a single argument-less `run()
-> i32` function (`WasmtimeSandbox::ENTRY_POINT = "run"`), and computes a
status code from data already inside the module or its linear memory. Network
access, file access, and env var access via WASI are **not available yet** —
`CapabilitySet` exists today as the declared policy surface for a future
capability-gated WASI host-function implementation, not as an enforced
allowlist at the sandbox boundary. Don't rely on `CapabilitySet` to grant a
running WASM module actual network/FS access; it doesn't, yet.

## Fuel limits

`FuelLimit` (`crates/familyclaw-sandbox/src/fuel.rs`) is `Limited(u64)` or
`Unlimited` (default: `Limited(1_000_000)` — `FuelLimit::DEFAULT_BUDGET`).
`WasmtimeSandbox` is constructed with `consume_fuel(true)`, so every WASM
operation costs fuel and an infinite loop is stopped by
`SandboxError::FuelExhausted` rather than hanging the host process. The
`FuelMeter` type carries the pure accounting logic (consume, remaining,
is_exhausted) independent of wasmtime, so budget behavior is unit-testable
without compiling the sandbox backend.

Set a fuel limit explicitly per request:

```rust
use familyclaw_sandbox::{CodeSandbox, SandboxRequest, FuelLimit, CapabilitySet};

let sandbox = familyclaw_sandbox::default_sandbox()?;
let request = SandboxRequest::new(wasm_bytes)
    .with_fuel_limit(FuelLimit::limited(1_000_000))
    .with_capabilities(CapabilitySet::deny_all());
let output = sandbox.execute(&request)?;
println!("fuel consumed: {}", output.fuel_consumed);
# Ok::<(), familyclaw_sandbox::SandboxError>(())
```

`Unlimited` removes the cap entirely and should only be used for fully trusted
code — the default is always limited.

## How to register and test a skill

1. **Define the manifest.** Either build a `SkillManifest` struct directly, or
   parse one from TOML/JSON (`SkillManifest::from_toml` /
   `SkillManifest::from_json`). Call `.validate()` before registering — it
   enforces the checks above and returns `ActionError::ManifestValidation` /
   `ActionError::SecretInManifest` / `ActionError::SignatureInvalid` on
   failure.
2. **Implement `ActionExecutor`** for your skill's execution body (see
   `fs_read.rs` or `web_fetch.rs` as templates for a local-filesystem or
   public-network skill respectively). Mark output taint correctly — see the
   taint rules below.
3. **Register it** with `SkillRegistry::register(manifest)`
   (`crates/familyclaw-actions/src/registry.rs`). Registration re-validates
   the manifest and rejects duplicate skill IDs.
4. **Drive it through `Pipeline`** (`crates/familyclaw-actions/src/skills/mod.rs`),
   which wires the registry, task queue, policy layer, approval ledger,
   executor, proof bundle builder, and audit collector together end to end.
   The pipeline resolves `required_approval` from the manifest and either
   completes the task (`TaskStatus::Done`) or parks it
   (`TaskStatus::NeedsApproval`).
5. **Test it** the same way the shipped skills are tested: unit tests in the
   skill's own module (`#[cfg(test)] mod tests` — see `fs_read.rs`,
   `web_fetch.rs`, `manifest.rs`, `policy.rs` for the pattern), plus, for
   sandboxed WASM skills, integration tests under
   `crates/familyclaw-sandbox/tests/sandbox_integration.rs` (requires the
   `wasmtime` feature: `cargo test -p familyclaw-sandbox --features wasmtime`).
   Fixtures there are built inline with `wat::parse_str` — no external `.wasm`
   artifacts required.

## Taint / untrusted-output rules

- `ActionRequest.input_untrusted` marks whether the input to a skill came from
  an untrusted source (e.g. an MCP tool's output). If `true`, taint
  **propagates** into the `ActionResult` — the skill's own executor cannot
  wash it clean by marking its own output trusted.
- Skill output is **untrusted (tainted) by default**. `fs_read` only marks
  output trusted when the canonicalized path falls under an explicitly
  configured **trusted** root (e.g. the project's own files); anything else —
  including all `web_fetch` results — stays tainted.
- Tainted data must never be silently promoted to trusted context. This is
  Layer 3 of the security model (taint tracing) — see
  [`SECURITY_MODEL.md`](SECURITY_MODEL.md#layer-3--taint-tracing).
- Proof bundles never contain raw file bodies or full untrusted payloads —
  `fs_read`'s proof carries a path hash, byte size, and a short (≤120-byte)
  summary only.

## Related documents

| Document | Topic |
|---|---|
| [`SECURITY_MODEL.md`](SECURITY_MODEL.md) | All eight defense layers, including Layer 6 (sandbox) and Layer 2 (approvals) in full detail. |
| [`crates/familyclaw-sandbox/README.md`](../crates/familyclaw-sandbox/README.md) | Sandbox crate structure and feature flags. |
| [`LAYER_BOUNDARY.md`](LAYER_BOUNDARY.md) | Why skills never carry real credentials or private data in this repo. |

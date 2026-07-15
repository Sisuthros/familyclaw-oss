# Security Benchmark — containment proof (one command)

> **Scope.** This is a *single-metric* containment benchmark: poisoned/malicious
> skills and prompt-injection payloads produce **ZERO sandbox escapes** and
> **ZERO unapproved side effects**. It is **not** a full penetration test, not a
> fuzzing campaign, and not a throughput/latency benchmark. The honesty caveats
> in §4 are part of the artifact, not a footnote — the claim is deliberately
> narrow so it stays true and reproducible.

Every scenario drives the **real** sandbox / actions API — nothing that is under
test is mocked away. The run is deterministic (injected clock, WASM compiled from
fixed WAT text), so two runs produce a byte-for-byte identical scorecard.

## One-command repro

```bash
# Enforced run (real WASM sandbox: fuel gate + host-import denial):
cargo run -p familyclaw-bench --features wasmtime --bin bench -- security

# Default run (SEC3/SEC4 fully enforced; SEC1/SEC2 report the capability model
# and honestly mark real-WASM execution as SKIPPED without the feature):
cargo run -p familyclaw-bench --bin bench -- security
```

The process **exits non-zero (Err) if any scenario fails**, so CI can gate on it
(same pattern as the continuity bench). On success it writes:

- `crates/familyclaw-bench/out/SECURITY_SCORECARD.md`
- `crates/familyclaw-bench/out/security_scorecard.json`
- `docs/SECURITY_SCORECARD.md` (public copy)

Run the tests directly:

```bash
cargo test -p familyclaw-bench                     # includes security unit + integration tests
cargo test -p familyclaw-bench --features wasmtime # SEC1/SEC2 against the real wasmtime backend
```

## Scenarios and what each metric means

Each scenario asserts a measured **0** for its escape metric.

### SEC1 — fuel exhaustion (`sec1_fuel_exhaustion`)

A skill containing an infinite loop is compiled to WASM and run through the real
`WasmtimeSandbox` with a bounded fuel budget. Without the fuel gate this would
hang.

- `escapes` — deviations from the expected halt. **Target 0.**
- `halted_by_fuel` — `1` when execution returned `SandboxError::FuelExhausted`
  (the loop was cut off, not left running).

Without the `wasmtime` feature the scenario is honestly marked SKIPPED
(`skipped_no_wasmtime = 1`) — it does **not** claim to have executed WASM.

### SEC2 — capability denial (`sec2_capability_denial`)

Proves the deny-by-default capability model on two levels: (1) `CapabilitySet::deny_all()`
denies network (incl. `169.254.169.254` cloud-metadata), filesystem, and
env-var access via the public API, and an explicit grant stays host-specific;
(2) with `wasmtime`, a WASM module requiring host imports (e.g. network) is
rejected because no capability links the import.

- `escapes` — **Target 0.**
- `capabilities_denied` / `capabilities_checked` — must be equal (every
  requested capability was denied).

### SEC3 — SSRF / prompt-injection (`sec3_ssrf_prompt_injection`)

Runs the real `WebFetchSkill` / `WebSearchSkill` with malicious payloads:
internal IPs (`127.0.0.1`, `10/8`, `172.16/12`, `192.168/16`, CGNAT `100.64/10`),
the cloud-metadata endpoint (`169.254.169.254`), IPv6 loopback/link-local,
`localhost`, non-http schemes (`file:`, `gopher:`, `data:` with an injected
"ignore-previous-instructions" body), plus a whitespace/injection search query.
The SSRF guard rejects these **before any network call** (literal IP / scheme /
host classification), so the scenario is network-free and deterministic.

- `escapes` — **Target 0.**
- `blocked` / `payloads` — must be equal (every payload refused).

### SEC4 — unapproved side effect (`sec4_unapproved_side_effect`)

A high-risk side effect is gated by the real `ApprovalLedger`. Without an
approval, consuming a phantom approval fails closed (0 executions). With a
payload-hash-bound approval, the first consume succeeds (exactly 1 execution), a
second consume is refused (one-shot), and a tampered payload is refused (hash
binding).

- `executions_without_approval` — **Target 0** (fail-closed).
- `executions_with_approval` — **Target exactly 1**.
- `escapes` — unauthorized executions (no-approval + reuse + tamper). **Target 0.**

## 4. What this does NOT claim (honesty-as-product-asset)

1. **NOT a full pentest / fuzzing campaign.** It measures ONE invariant:
   escapes and unapproved side effects under poisoned-skill / injection inputs.
2. **NOT a complete SSRF proof.** SEC3 covers the classification guard
   (scheme / host / literal IP) without network. A full DNS-rebinding test would
   require a mock resolver; the runtime skill has an execute-time resolved-IP
   recheck (see `web_fetch.rs`), which this bench does not exercise over a real
   network — stated so the boundary is explicit.
3. **NOT a WASM-escape guarantee for arbitrary payloads.** SEC1/SEC2 prove the
   fuel gate halts a runaway loop and host-import modules are denied; they do not
   claim wasmtime is free of all sandbox-escape CVEs.
4. **SEC1/SEC2 need the `wasmtime` feature** to run the real backend. Without it
   they honestly report SKIPPED for real-WASM execution while still proving the
   capability model. CI should gate on the `--features wasmtime` run.
5. **NOT a throughput / latency / cost benchmark.** No performance claim is made.

**Bottom line:** malicious skills and injection payloads, run against the real
sandbox and approval gate, produced **0 escapes and 0 unapproved side effects** —
stated as exactly that, no more.

# FamilyClaw Security Scorecard

> Single-metric containment proof: poisoned skills and prompt-injection payloads produce ZERO sandbox escapes and ZERO unapproved side effects. Not a full pentest — see docs/SECURITY_BENCH.md for scope and caveats.

- **Subject:** familyclaw-security
- **Reference clock:** 2026-06-04T12:00:00.000Z
- **Sandbox backend:** wasmtime (host-import denial + fuel cap)
- **Overall:** PASS

## sec1_fuel_exhaustion — PASS

| Metric | Value |
|--------|-------|
| escapes | 0.0000 |
| halted_by_fuel | 1.0000 |

- infinite-loop wasm skill run through real WasmtimeSandbox
- halted by fuel gate: fuel exhausted: budget 10000, required 10001
- escapes=0 (target 0)

## sec2_capability_denial — PASS

| Metric | Value |
|--------|-------|
| capabilities_checked | 4.0000 |
| capabilities_denied | 4.0000 |
| escapes | 0.0000 |

- deny-by-default capability model + real sandbox host-import denial
- denied network:169.254.169.254: true
- denied network:any: true
- denied fs:/etc/passwd: true
- denied env:AWS_SECRET_ACCESS_KEY: true
- explicit grant stays host-specific: true
- host-import module denied by real sandbox: sandbox setup failed: module requires host imports, which are not granted by the current capability set
- escapes=0 (target 0)

## sec3_ssrf_prompt_injection — PASS

| Metric | Value |
|--------|-------|
| blocked | 13.0000 |
| escapes | 0.0000 |
| payloads | 13.0000 |

- SSRF/prompt-injection payloads run through real web_fetch/web_search skills
- web_fetch: blocked 12/12 internal/metadata/non-http payloads
- web_search: whitespace/injection query blocked=true (host is fixed, not user-controlled)
- blocked=13/13, escapes=0 (target 0)

## sec4_unapproved_side_effect — PASS

| Metric | Value |
|--------|-------|
| escapes | 0.0000 |
| executions_with_approval | 1.0000 |
| executions_without_approval | 0.0000 |

- high-risk side effect gated by real ApprovalLedger (payload-hash bound, one-shot)
- without approval: executions=0 (fail-closed, target 0)
- with payload-bound approval: executions=1 (target exactly 1)
- reuse blocked (one-shot): true
- tampered payload blocked (hash binding): true
- escapes=0 (target 0)


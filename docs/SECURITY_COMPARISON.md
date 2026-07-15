# Security posture comparison: FamilyClaw vs OpenClaw vs Hermes Agent

> Honest, structural comparison — not a marketing table. Every FamilyClaw row
> is backed by a test, a CI gate, or a named module in this repo (linked
> below). Every competitor row is attributed to public reporting; FamilyClaw
> did not independently audit OpenClaw or Hermes Agent source. If a claim
> here turns out to be wrong or stale, open an issue — this document is
> falsifiable by design, same as [STATUS.md](../STATUS.md).

Competitor facts below are drawn from public reporting, 2026 (CVE records,
public scan results, and project-reported incidents current as of mid-2026).
FamilyClaw facts are drawn from this repository as of `v1.2.0` — see the
"Evidence" column for the exact module or doc.

---

## Structural comparison

| Concern | OpenClaw (public reporting, 2026) | Hermes Agent (public reporting, 2026) | FamilyClaw | Evidence |
|---|---|---|---|---|
| Memory safety | — | — | `unsafe` forbidden workspace-wide (compile-time gate, not policy) | `Cargo.toml` workspace lint config; [STATUS.md](../STATUS.md) |
| Known critical CVEs | CVE-2026-25253 one-click RCE, CVSS 8.8; plus 2 separate command-injection advisories | CVE-2026-7113, CVSS 5.6 | None reported | — |
| Default network exposure | Auth disabled by default; ~21,600 exposed instances found by Censys scan | Not reported as a default-exposure issue | Channel-less `serve` mode runs with no external network surface beyond `/healthz`, `/readyz`, `/metrics`; no default auth-off remote control path | README "Run the gateway in 5 minutes" section |
| Credential storage | Plaintext credential storage reported | Not the primary reported issue | No credentials in repo; Layer B (private profiles, keys, tokens) loaded only at runtime via `FAMILYCLAW_PROFILE_DIR`, never committed | [docs/LAYER_BOUNDARY.md](LAYER_BOUNDARY.md); `scripts/audit-layer-b.sh` |
| Third-party skill/code execution | ClawHub marketplace: 335 malicious skills found, ~17% of analyzed skills malicious | Skill poisoning named as the top concern; no signed skill provenance | WASM sandbox, deny-by-default host imports, fuel-gated (Wasmtime) | `familyclaw-sandbox`; `sandbox_integration.rs` (CI, `wasmtime` feature) |
| Skill provenance | No signing scheme reported for ClawHub skills | No signed skill provenance; no immutable log of skill promotions | Ed25519-signed external skill manifests; unsigned/invalid-signature skills fail closed at registration (never enter the registry) | `familyclaw-actions` → `manifest::SkillManifest::validate`; [docs/SECURITY_MODEL.md](SECURITY_MODEL.md) "Signed external skills" |
| Prompt injection / indirect hijack | "ClawJacked" — indirect prompt injection used to hijack a local instance | Not the primary reported issue | Taint tracing: tool outputs from network fetches and untrusted reads are marked tainted and never silently promoted to trusted context | [docs/SECURITY_MODEL.md](SECURITY_MODEL.md) Layer 3 |
| Approval gate on risky actions | Not reported as a structural gate | Not reported as a structural gate | Every skill declares risk + approval policy in its manifest (never from task payload); high-risk actions require approval; pending approvals are TTL-bound, one-shot, payload-hash-bound | `familyclaw-actions` → `approval`, `policy`; [docs/SECURITY_MODEL.md](SECURITY_MODEL.md) Layer 2 |
| Proof/audit redaction | Not reported | Not reported | Proof bundles and audit records pass through `redact_free_text`; proofs record hashes/counts, never raw secrets or full file bodies | `familyclaw-actions` → `proof`, `redact_free_text` |
| Layer/boundary enforcement in CI | Not applicable (no public/private split reported) | Not applicable | Layer A / Layer B boundary enforced by `scripts/audit-layer-b.sh` in CI on every merge | [docs/LAYER_BOUNDARY.md](LAYER_BOUNDARY.md); `.github/workflows/ci.yml` `layer-b-audit` job |
| Crash / duplicate side-effect handling | Not reported as benchmarked | Session-resume failures and memory-compression failures reported | At-most-once external side-effect dispatch under crash: `side_effect_overcount = 0` at every crash point, benchmarked against LangGraph | `familyclaw-actions` → `dispatch_outbox`; [docs/SECURITY_MODEL.md](SECURITY_MODEL.md) Layer 7; [bench-competitors/langgraph/](../bench-competitors/langgraph/README.md) |
| Self-improvement / self-modification safety | Not applicable (not a self-improving agent per public reporting) | Uncalibrated self-assessment of its own improvement loop — "the agent learned that X" is not a provable claim | Growth loop has **no apply path** by construction (cannot mutate skill, policy, or permission); promotion evidence is a `TimelineDiff` between deterministic replay runs, fail-closed on missing/regressed evidence | `familyclaw-growth` → `Proposal`, `ProposalStore`, `evidence::evaluate_for_approval`; [STATUS.md](../STATUS.md) roadmap table |
| Time-travel / deterministic rewind | Not reported | Not reported | Deterministic journal rewind (inspect), fork with audit marker, counterfactual dry-run with no dispatch path by construction, timeline diff | `familyclaw-durable` → `TimeMachine`; [STATUS.md](../STATUS.md) "Time Machine" row |

---

## What FamilyClaw does NOT claim

Honesty cuts both ways. This is not a claim of superiority on every axis:

- **No formal verification.** `unsafe`-forbidden and fail-closed defaults are
  structural guarantees enforced by the compiler and CI, not a machine-checked
  proof of security properties. Logic bugs in safe Rust are still possible.
- **Smaller deployment footprint than the competitors it's compared against.**
  OpenClaw and Hermes Agent each have tens of thousands of real-world
  deployments generating incident reports; FamilyClaw does not have that
  battle-testing yet. A large known-CVE count partly reflects a large,
  actively-attacked install base — absence of reported FamilyClaw CVEs is not
  proof of equivalent scrutiny.
- **Narrower channel surface today.** Discord inbound is live; Telegram,
  WhatsApp, and Signal are feature-flagged and less exercised in the wild than
  OpenClaw's or Hermes Agent's channel integrations.
- **No public bug bounty or third-party security audit yet.** The claims in
  this document are backed by this repo's own tests and CI gates, not an
  independent audit.
- **Growth-loop apply path is intentionally unshipped**, not proven safe in
  production — see the roadmap table in [STATUS.md](../STATUS.md). The
  evidence layer described above only gates a promotion *decision*; it does
  not yet gate a live apply step, because that step does not exist yet.

---

## Reproduce this yourself

Every FamilyClaw row above traces to a runnable command, not a slide:

```bash
# Full test suite (23 crates)
cargo test --workspace --features discord

# All-features gate (matches CI)
cargo test --workspace --all-features

# WASM sandbox e2e (fuel exhaustion + denied capabilities)
cargo test --workspace --features familyclaw-gateway/wasmtime

# Layer A / Layer B leak audit (matches CI)
bash scripts/audit-layer-b.sh

# At-most-once dispatch under crash, vs LangGraph
cd bench-competitors/langgraph && python -m venv .venv \
  && .venv/Scripts/python.exe -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
cargo run -p familyclaw-bench -- s1    # side_effect_overcount = 0, PASS

# Deterministic continuity scorecard (8 scenarios, includes provenance gate)
cargo run -p familyclaw-bench --bin bench -- all
```

See also [docs/SECURITY_MODEL.md](SECURITY_MODEL.md) (full eight-layer model)
and [STATUS.md](../STATUS.md) (source of truth for what is verified today).

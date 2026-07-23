# FamilyClaw — Commercial Offer

**A Rust-native reliability runtime for long-running AI agents that take real external actions.**

> Wedge: *We make AI agents safe enough to act in the real world.*

FamilyClaw is a Rust workspace (23 crates, v1.2.0, MIT-licensed, `unsafe` forbidden workspace-wide, ~1905 tests — see [STATUS.md](../STATUS.md)) built around one problem: when an AI agent takes real external actions — sending, paying, provisioning, writing to a customer's systems — a crash, retry, or race condition can cause it to do the wrong thing twice, or do it without approval. FamilyClaw hardens that path with **at-most-once external side-effect dispatch under crash**, **fail-closed behavior in the intent-only window**, and content-hash-bound approvals — not a claim of universal exactly-once *completion*.

This document defines two commercial offers built directly on FamilyClaw's tested capabilities. It is honest about what is proven and what is not. There are no production deployments, no customers, no revenue, and no certifications yet — these are the first offers, and early clients are founding pilots.

---

## What FamilyClaw Actually Does (Verified Capabilities)

Every capability below is implemented and tested in the runtime. When an offer references a capability, it is one of these:

- **Crash-safe dispatch** — external actions survive process crashes without silent loss.
- **Durable deterministic replay** — a journal-and-replay model that reconstructs state without re-executing closures, in pure Rust, with no external services required.
- **Duplicate-action prevention** — at-most-once external dispatch under tested crash windows.
- **Content-hash-bound approval gates** — TOCTOU-safe, fail-closed. If the payload changes after approval, dispatch fails with `ApprovalPayloadMismatch` rather than acting on unapproved content.
- **Auditability** — `/turns/audit` and `/approvals` HTTP endpoints, plus Prometheus metrics.
- **Persistent memory** — Ebbinghaus decay for recall, plus a nightly "dream" consolidation that reshapes the memory *set* (merges duplicates, grounds relative dates). (Decay changes recall output; consolidation reshapes the set — these are separate mechanisms.)
- **Provider/model failover** — on 401/403, rotate within the key pool before cooling a provider; on 429, an escalating cooldown ladder (60s / 5m / 25m / 1h); an auth ladder (5m / 30m / 2h / 6h); a two-pass strategy (healthy providers first, then a last-resort pass that retries all so the system never hangs); fail-closed on non-retryable `Parse` / `InvalidTool` errors.
- **WASM sandbox** — Wasmtime + Cranelift, deny-by-default, with fuel metering.
- **Self-modification is impossible by construction** — there is no `apply()`; the growth/WASM layer cannot rewrite itself.

**What is proven, and how far:** an 8/8 deterministic scorecard (`s1`..`s8`), and a genuine pinned-LangGraph 1.2.6 crash-window benchmark. The benchmark measures **duplicate external side-effect dispatch under specific crash windows only** — it does **not** measure throughput, latency, usability, or model quality. Proof to date is local; hosted CI may be billing-blocked, so no hosted-CI-passed claim is made.

**Demos (deterministic, no keys, no network, self-asserting):**

```bash
# Flagship: two agents sharing persistent memory
cargo run -p familyclaw-agent --example two_agents_memory

# Crash proof: replay after a full crash
cargo run -p familyclaw-agent --bin crash_replay -- full

# Scorecard: the 8/8 deterministic suite
cargo run -p familyclaw-bench --bin bench -- all
```

Repo access is **private demo access on request**.

---

## Offer A — AI Agent Reliability Review

**Price: EUR 750–1500.** A structured audit of one AI agent workflow and its failure modes.

### Ideal customer

A team that already runs, or is about to ship, an AI agent that takes **real external actions** (sending messages, making payments, provisioning resources, writing to third-party systems) and is worried about what happens on a crash, retry, or race.

### Qualification criteria

- You have at least one concrete agent workflow that performs external side effects.
- You can describe the workflow's architecture and share (redacted) logs or code paths for the action-dispatch and retry logic.
- You have someone who can answer technical questions in a working session.

### Scope

- **In scope:** one agent workflow. Architecture review of how it dispatches external actions, handles retries, and gates approvals.
- **Out of scope:** implementation, code changes, deployment, or building a proof of concept. That is Offer B.

### Exclusions

- No code is written or merged.
- No access to your production systems is required or requested.
- No handling of your customer secrets.
- No model-quality, prompt-quality, throughput, or latency assessment — this is a reliability and correctness review, not a performance benchmark.

### Input required from you

- A description of the one workflow to review.
- Read access (redacted is fine) to the relevant code paths and/or logs for action dispatch, retry, and approval.
- One technical contact for a working session.

### Deliverables

1. **Workflow architecture review** — how the agent currently dispatches external actions.
2. **Failure-mode map** — where crashes, retries, and races can cause wrong or duplicate actions.
3. **Duplicate-action analysis** — where at-most-once dispatch is and isn't guaranteed today.
4. **Approval and audit recommendations** — where content-hash-bound approval gates and audit trails would close gaps.
5. **Prioritized remediation plan** — ordered by risk and effort.

### Timeline

Approximately **2–3 days** from receiving inputs.

### Acceptance criteria

- Delivery of the written review covering all five deliverables.
- The failure-mode map references your actual workflow, not a generic template.
- The remediation plan is prioritized and actionable.

### Upgrade path

Review → **Sprint** (Offer B: implement the top remediation on one workflow) → ongoing support.

### Risks

- If the workflow is larger or more entangled than described, the review may narrow to the highest-risk subset within the timebox.
- Recommendations are only as good as the inputs; incomplete code/log access limits depth.

### What must be separately scoped

- Any implementation, hardening, or proof of concept (that is Offer B).
- Review of more than one workflow.
- Anything requiring access to production systems or customer secrets.

---

## Offer B — AI Agent Reliability Sprint (Founding Pilots)

**Price: EUR 1500–3500.** A focused **5-day** pilot that hardens **one real agent workflow** and delivers a working proof of concept.

### Ideal customer

A team with one high-value agent workflow that takes real external actions, that wants it made crash-safe, idempotent, and approval-gated — and wants working proof, not a slide deck. Early clients are **founding pilots**.

### Qualification criteria

- You have completed a Reliability Review, or can clearly define one workflow and its failure modes up front.
- The workflow's external actions can be exercised against a **test/staging** target (not live production) for the pilot.
- You can commit a technical contact for the sprint week.

### Scope

- **In scope:** one real workflow, end to end — crash/retry hardening, idempotent external-action design, an approval gate, audit evidence, and a provider failover review, delivered as a working proof of concept.
- **Out of scope:** hardening multiple workflows; production cutover; long-term operation.

### Exclusions

- No deployment to your production environment (proof of concept runs against test/staging).
- No handling of live customer secrets or access to customer production systems unless separately scoped **and approved**.
- No throughput/latency tuning or model-quality work.

### Input required from you

- One defined workflow and its external actions.
- A test/staging target the external actions can safely exercise against.
- Relevant code access and a technical contact available during the sprint.

### Deliverables

1. **One real workflow** hardened end to end.
2. **Crash/retry hardening** — using crash-safe dispatch and durable deterministic replay.
3. **Idempotent external-action design** — at-most-once dispatch under crash windows.
4. **Approval gate** — content-hash-bound, TOCTOU-safe, fail-closed.
5. **Audit evidence** — via the `/turns/audit` and `/approvals` endpoints and Prometheus metrics.
6. **Provider failover review** — mapping your providers onto the rotation/cooldown/auth ladders and fail-closed behavior.
7. **Working proof of concept** — runnable, deterministic where possible.
8. **Final handover** — documentation of what was built, what is proven, and the recommended next steps.

### Timeline

**5 days.**

### Acceptance criteria

- The hardened workflow demonstrates at-most-once external dispatch across the tested crash windows.
- The approval gate fails closed on a payload mismatch (`ApprovalPayloadMismatch`).
- Audit evidence is retrievable via the endpoints/metrics.
- The proof of concept runs and self-asserts as agreed at kickoff.

### Upgrade path

Sprint → **ongoing support** (retainer / additional workflows / production-readiness work), scoped separately.

### Risks

- Five days is a timebox. If the workflow proves larger than scoped, the sprint delivers a hardened, correct **subset** rather than a rushed whole.
- The proof of concept demonstrates reliability properties (idempotency, approval, audit) — not production performance or scale.
- Proof to date and within the pilot is local/test; hosted-CI or production-scale validation is separate work.

### What must be separately scoped

- Production deployment or cutover.
- Additional workflows beyond the one.
- Any access to production systems or handling of customer secrets.
- Throughput, latency, scale, or model-quality work.

---

## Pricing Strategy Note

Both offers are **anchored at the top of their range** with fully defined scope: EUR 1500 for a Review and EUR 3500 for a Sprint represent the complete, defined engagement. **The lower end of each range (EUR 750 / EUR 1500) is the concession for a narrower pilot** — a smaller workflow, tighter scope, or reduced deliverable set. Price down by removing scope, never by discounting the full scope.

---

## Contact

**Commercial / pilots:** open a private request via
[GitHub Discussions](https://github.com/Sisuthros/familyclaw/discussions)
(category: *Pilots*) or email `pilots@familyclaw.dev` (alias — responses
within the pilot SLA in `docs/commercial/PILOT_SLA.md`).

**Security:** [GitHub Security Advisories](https://github.com/Sisuthros/familyclaw/security/advisories/new)
— see also `docs/BUG_BOUNTY.md`.

Repository may be private during evaluation — **private demo access available
on request** through the channels above.

---

*FamilyClaw v1.2.0 · Rust workspace · 23 crates · MIT · `unsafe` forbidden workspace-wide · ~1905 tests (see STATUS.md) · 8/8 deterministic scorecard. No production deployments, customers, or certifications yet — early clients are founding pilots. Claims in this document are limited to tested capabilities.*

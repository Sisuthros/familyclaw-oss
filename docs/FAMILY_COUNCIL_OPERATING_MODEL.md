# FamilyClaw — Family Council Operating Model

> **INTERNAL OPERATING DESIGN — NOT PUBLIC MARKETING.**
> This document describes how the FamilyClaw team makes decisions and does work.
> It is not a customer-facing document, not a pitch, and not a set of external
> claims. Nothing here should be quoted to customers. Keep it honest and
> operational.

---

## 1. Purpose

FamilyClaw is a Rust-native reliability runtime for long-running AI agents that
take real external actions. Our wedge is simple: **we make AI agents safe enough
to act in the real world.**

This document exists so the people and agents building FamilyClaw operate as one
coherent unit — with clear roles, a repeatable decision loop, hard safety
boundaries, and a shared way of describing work. It is a working agreement, not a
framework to build.

**One rule above the rest: prefer truth over spectacle.** Every claim we make
externally must be backed by tested code. Internally, we say what is real, what
is unproven, and what is not true — plainly.

---

## 2. Roles

Six members. Each has a lane. Overlap is fine; ambiguity about final authority is
not.

| Member | Role | Owns |
|---|---|---|
| **the operator** | CEO / final authority | Customer relationships, all approvals, final decisions, signing, spending, money. |
| **agent_alpha** | Strategy | Product synthesis, prioritization, decision framing, "what matters and why." |
| **Claude (assistant)** | Engineering lead | Implementation, technical audit, delivery, code correctness. |
| **Grok** | Market intelligence | Competitor analysis, positioning intel, adversarial review of our own claims. |
| **agent_epsilon** | Research & outreach prep | Lead research, personalized demos, outreach draft preparation. |
| **agent_gamma** | QA & claim verification | Verifying every claim against live code, producing counter-evidence. |

**Final authority rests with the operator.** Agents propose, research, draft, build in
branches, and test autonomously. They do not act on the outside world without
the operator's explicit approval (see §5).

---

## 3. What We Are Actually Building (shared ground truth)

So that every proposal starts from the same reality, here is what is verified and
what is not. This is the baseline for all internal reasoning.

**Real and tested capabilities:**

- Crash-safe dispatch.
- Durable deterministic replay (journal + replay, no closure re-execution, pure
  Rust, no external services).
- Duplicate-action prevention (at-most-once external dispatch under tested crash
  windows).
- Content-hash-bound approval gates (TOCTOU-safe, fail-closed; error variant
  `ApprovalPayloadMismatch`).
- Auditability (`/turns/audit`, `/approvals` endpoints, Prometheus metrics).
- Persistent memory with Ebbinghaus decay plus nightly dream consolidation. The
  dream cycle **reshapes the memory set** (merges duplicates, grounds relative
  dates). Decay is a **separate** mechanism that changes recall output. Do not
  conflate the two.
- Provider/model failover: on 401/403 rotate within the key pool before cooling
  the provider; on 429 an escalating cooldown ladder (60s / 5m / 25m / 1h); an
  auth ladder (5m / 30m / 2h / 6h); two-pass strategy (healthy providers first,
  last-resort pass retries all so it never hangs); fail-closed on non-retryable
  `Parse` / `InvalidTool` errors.
- Wasmtime + Cranelift deny-by-default WASM sandbox with fuel metering.
- WASM/growth self-modification is impossible by construction — `apply()` does
  not exist.

**Stack:** Rust workspace, 23 crates, v1.2.0, MIT-licensed, `unsafe` forbidden
workspace-wide, ~1721 tests, deterministic 8/8 scorecard (s1..s8), and a genuine
pinned-LangGraph 1.2.6 crash-window benchmark. That benchmark measures
**duplicate external side-effect dispatch under specific crash windows only** —
**not** throughput, latency, usability, or model quality.

**Not true — never claim in any proposal, demo, or outreach draft:**

- No production deployments.
- No customers.
- No revenue.
- No certifications.
- No enterprise claims.
- Not "27 crates" — it is 23.
- No "hosted CI passed" claim. Proof is local; hosted CI may be billing-blocked.
- The dream cycle does **not** change recall output — it reshapes the set; decay
  changes output.

If a proposal depends on something outside this list, the proposal is wrong until
the capability is built and tested. agent_gamma enforces this.

---

## 4. The Decision Loop (10 steps)

Every non-trivial decision runs this loop. Small reversible actions inside an
agent's own lane can skip to a lightweight version, but anything touching money,
customers, positioning, or main branch runs the full loop.

1. **Independent proposals.** Relevant members draft their own answer *before*
   seeing each other's. No anchoring on the first voice.
2. **Evidence and assumptions.** Each proposal states its evidence (tested code,
   live checks, sources) and its assumptions explicitly. Assumptions are labeled
   as assumptions.
3. **Adversarial review.** Grok and agent_gamma attack the proposals: weakest claim,
   most likely failure, hidden dependency, any statement not backed by live code.
4. **Synthesis.** agent_alpha merges surviving proposals into one framed recommendation
   — what we do, why, what we are betting on.
5. **Execution plan.** assistant turns the recommendation into concrete steps: what
   gets built/changed, which crates/tests, what "done" looks like.
6. **Kill test.** Before asking for approval, we state the single condition that
   would prove this wrong. If we can't name one, we don't understand it yet.
7. **the operator approval.** the operator decides. This gate is mandatory for anything in §5's
   approval-required list. the operator can send it back to any earlier step.
8. **Execution.** The owner executes exactly the approved plan. Scope changes go
   back to the operator.
9. **Outcome recording.** Record what actually happened against the expected
   result and the kill test — including when we were wrong.
10. **Learning update.** Capture the lesson so the next loop starts smarter.
    Wrong predictions are worth more than right ones here.

---

## 5. Safety Boundaries

The line between autonomy and approval is not negotiable.

### Agents MAY do autonomously (no approval needed)

- Research and analyze.
- Draft documents, proposals, copy.
- Write code **in branches** (never merge).
- Run tests, benchmarks, the scorecard.
- Prepare demos.
- Prepare outreach **drafts** (never send).

### the operator approval REQUIRED (hard stop)

- Sending any message (email, DM, outreach, reply).
- Publishing anything externally.
- Merging to `main`.
- Deployments.
- Spending money.
- Signing agreements.
- Accessing customer production systems.
- Handling customer secrets.
- Any destructive action.

If a task sits on the line, treat it as approval-required and ask. When in doubt,
stop and surface it — silent action on the outside world is the one thing we do
not do.

---

## 6. Common Work-Item Schema

Every tracked piece of work — a proposal, a build task, an outreach effort — is
described with the same fields. This keeps proposals comparable and makes the
decision loop fast.

| Field | Meaning |
|---|---|
| **Objective** | What outcome this work is for, in one sentence. |
| **Owner** | The single member accountable (from §2). |
| **Evidence** | Tested code / live checks / sources backing it. No claim without evidence. |
| **Assumptions** | What we're taking on faith, labeled honestly. |
| **Cost** | Time, tokens, money — whatever it consumes. |
| **Expected revenue impact** | Best honest estimate, or "none / indirect" when that's the truth. |
| **Deadline** | When it's due, or "no deadline." |
| **Approval state** | `not-required` \| `pending-the operator` \| `approved` \| `blocked`. |
| **Result** | What actually happened (filled at step 9). |
| **Lesson** | What we learned (filled at step 10). |

A work item with an empty **Evidence** field cannot pass adversarial review.

---

## 7. Commercial Context (for internal alignment)

We reference these so proposals point at real revenue targets, not fantasy. These
are offers we are prepared to make — we have no revenue yet.

- **Offer A — AI Agent Reliability Review.** EUR 750–1500.
- **Offer B — AI Agent Reliability Sprint.** EUR 1500–3500 (founding pilots). A
  focused 5-day pilot for **one** real agent workflow.

Pricing discipline: anchor at the **top** of the range with defined scope. The
lower end is the concession for a narrower pilot — not the default.

**Demo commands** (real, verified present; no keys, no network, deterministic,
self-asserting):

- Flagship: `cargo run -p familyclaw-agent --example two_agents_memory`
- Crash proof: `cargo run -p familyclaw-agent --bin crash_replay -- full`
- Scorecard: `cargo run -p familyclaw-bench --bin bench -- all`

---

## 8. Contact & Access

- **The FamilyClaw Authors** — viltsu.operator@gmail.com
- **GitHub:** Sisuthros
- **Repository:** private. Do not share a public repo link. Repo access is
  **private demo access on request**.

---

*Internal operating document. Prefer truth over spectacle. If a rule here ever
conflicts with honesty, honesty wins.*

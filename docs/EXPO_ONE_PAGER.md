# FamilyClaw

## A Rust-native reliability runtime for long-running AI agents that take real external actions.

**We make AI agents safe enough to act in the real world.**

---

## The problem

The moment an AI agent stops just *talking* and starts *acting* — charging a card, sending an email, calling an API, moving money — a whole class of failures becomes expensive and visible to your customers:

- It **double-charges** or **double-sends** because a retry re-ran an action that already happened.
- It **re-executes** completed side effects after a crash or restart.
- It **acts without approval** on a payload that changed between review and execution.
- It leaves **no audit trail**, so when something goes wrong you can't prove what it did or why.

For most teams, the model isn't the risk. The *execution layer* is.

---

## What FamilyClaw is

FamilyClaw is a Rust-native runtime that sits under your agent and makes its external actions reliable by construction. It provides crash-safe dispatch, durable deterministic replay, at-most-once external side effects, content-bound approval gates, full auditability, and resilient provider failover — all in a pure-Rust workspace with `unsafe` forbidden across the board. It is infrastructure for the part of an agent that touches the real world, not another agent framework.

---

## Five capabilities that matter

1. **Crash-safe durable replay** — a journal-and-replay design (pure Rust, no external services, no closure re-execution) that deterministically reconstructs agent state after a crash.
2. **Duplicate-action prevention** — at-most-once external dispatch under tested crash windows, so a restart never re-sends or re-charges.
3. **Content-hash-bound approval gates** — TOCTOU-safe and fail-closed; if the approved payload changes, execution stops with an `ApprovalPayloadMismatch` error instead of acting on the wrong thing.
4. **Audit trail** — `/turns/audit` and `/approvals` endpoints plus Prometheus metrics give you a defensible record of what the agent did and what was approved.
5. **Provider/model failover** — key-pool rotation on auth failures, escalating cooldown ladders on rate limits, and a two-pass healthy-first-then-last-resort strategy so the agent degrades instead of hanging, and fails closed on non-retryable errors.

*Built on a 23-crate Rust workspace (v1.2.0, MIT), `unsafe` forbidden workspace-wide, ~1721 tests, and an 8/8 deterministic scorecard. A pinned-LangGraph 1.2.6 crash-window benchmark measures duplicate external side-effect dispatch under specific crash windows.*

---

## Offer: The AI Agent Reliability Sprint

A focused **5-day pilot** that hardens **one real agent workflow** of yours against the failures above — the workflow where a double-charge, a duplicate send, or an unapproved action would actually hurt.

### Pilot deliverables

- A reliability review of the target agent workflow, focused on external side effects and approval boundaries.
- Crash-window analysis: where a restart could re-execute, double-charge, or double-send.
- A working demonstration of crash-safe replay and at-most-once dispatch applied to your workflow's shape.
- Approval-gate and audit-trail recommendations, mapped to concrete endpoints and metrics.
- A written summary of findings, risks, and the reliability gaps that remain.

### Founding-pilot price: EUR 1,500–3,500

Anchored at **EUR 3,500** for the full 5-day sprint with the defined scope above. The lower end is the concession for a narrower pilot covering a smaller slice of the workflow. Founding-pilot pricing — a limited early cohort.

> A lighter starting point is also available: **AI Agent Reliability Review** (EUR 750–1,500), a scoped assessment without the hands-on sprint.

---

## Try it yourself (no keys, no network, deterministic)

```bash
# Flagship: two agents with shared persistent memory
cargo run -p familyclaw-agent --example two_agents_memory

# Crash proof: replay across a crash window
cargo run -p familyclaw-agent --bin crash_replay -- full

# Scorecard: the deterministic 8/8 suite
cargo run -p familyclaw-bench --bin bench -- all
```

Each demo is self-asserting and runs offline.

---

## Contact

**The FamilyClaw Authors** — viltsu.operator@gmail.com
GitHub: **Sisuthros**
Repository is private — *private demo access on request.*

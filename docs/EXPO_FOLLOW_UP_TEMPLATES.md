# FamilyClaw — Expo Follow-Up Message Templates

> **Draft only — do NOT send without the operator's approval.**

These are reusable follow-up templates for after the Cyprus AI Expo. Fill in the
`[bracketed placeholders]` per contact. Keep every claim honest — nothing here
overstates what FamilyClaw is today.

**One-line positioning to lead with:** *FamilyClaw is a Rust-native reliability
runtime for long-running AI agents that take real external actions. We make AI
agents safe enough to act in the real world.*

**Contact block (reuse in any template):**
The FamilyClaw Authors — viltsu.operator@gmail.com — GitHub: Sisuthros.
Repo is private; private demo access available on request.

---

## 1. Strong lead (specific workflow + fear identified)

**Subject:** Making [their agent workflow] safe enough to run unattended

Hi [Name],

Great talking at the Expo about [their specific workflow] — the part that stood
out was your concern about [the specific failure they fear, e.g. "the agent
firing the same external action twice after a crash"]. That is exactly the
problem FamilyClaw is built to remove: at-most-once external dispatch under
tested crash windows, content-hash-bound approval gates that fail closed, and
durable deterministic replay so a restarted agent picks up its journal instead
of re-running side effects.

I'd like to propose a **5-day AI Agent Reliability Sprint** on that one
workflow: a scoped, defined deliverable — we harden [their workflow], prove the
crash and duplicate-action behavior, and hand you a reproducible test you own.
Founding-pilot pricing is **EUR 3500** for the full scope (narrower slices can
be scoped down from there).

Would a 30-minute call [next week] work to define the scope?

Best,
the operator — viltsu.operator@gmail.com

---

## 2. Technical peer (share the reproducible demo + benchmark honesty)

**Subject:** Reproducible crash-safety demo you can run locally

Hi [Name],

Following up from the Expo — you asked how the reliability claims hold up in
practice. Everything runs offline, no keys, no network, and each demo
self-asserts:

```
cargo run -p familyclaw-agent --example two_agents_memory      # flagship
cargo run -p familyclaw-agent --bin crash_replay -- full       # crash proof
cargo run -p familyclaw-bench --bin bench -- all               # deterministic scorecard (8/8)
```

Honest framing on the benchmark: it's a pinned-LangGraph 1.2.6 crash-window
comparison that measures **duplicate external side-effect dispatch under
specific crash windows only** — not throughput, latency, usability, or
model quality. I don't want to imply more than it tests.

Stack, if useful: Rust workspace, 23 crates, v1.2.0, MIT, `unsafe` forbidden
workspace-wide, ~1721 tests. Sandbox is Wasmtime + Cranelift, deny-by-default,
with fuel metering. Happy to give you private demo access to poke at it.

Best,
the operator — GitHub: Sisuthros — viltsu.operator@gmail.com

---

## 3. Investor (the wedge + why Rust-native reliability is a real gap)

**Subject:** The reliability gap under agents that take real actions

Hi [Name],

Thanks for the conversation at the Expo. The wedge for FamilyClaw is narrow and
deliberate: **we make AI agents safe enough to act in the real world.** As
agents move from chat to taking real external actions, the failure mode that
matters is no longer a bad answer — it's a duplicated payment, a re-sent
message, a re-run destructive action after a crash.

FamilyClaw is a Rust-native runtime that addresses that layer directly:
crash-safe dispatch, durable deterministic replay with no external services,
at-most-once external dispatch under tested crash windows, content-hash-bound
approval gates that fail closed, and a deny-by-default WASM sandbox. Rust-native
matters here because reliability and memory-safety are structural, not
bolted-on — `unsafe` is forbidden workspace-wide and self-modification is
impossible by construction.

To be straight with you: no production deployments, customers, revenue, or
certifications yet. This is a technical foundation and an early commercial
motion, not traction. If the reliability layer is a thesis you're tracking, I'd
value 20 minutes to walk you through it.

Best,
the operator — viltsu.operator@gmail.com

---

## 4. Platform / cloud contact (NVIDIA, cloud, agent platform)

**Subject:** A reliability layer for agents on [their platform]

Hi [Name],

Good to connect at the Expo. As [their platform] hosts more agents that take
real external actions, the hard part shifts from model quality to execution
reliability — making sure an agent that crashes mid-action doesn't double-fire
its side effects.

FamilyClaw is a Rust-native runtime focused on exactly that layer:
at-most-once external dispatch under tested crash windows, durable
deterministic replay (pure Rust, no external services), content-hash-bound
approval gates that fail closed, provider/model failover with cooldown ladders,
and a Wasmtime + Cranelift deny-by-default sandbox with fuel metering. It's
built to sit under agent execution, not replace the model or the platform.

I think there's a clean integration angle for reliability guarantees on top of
[their platform]. Would someone on your agent-infra side be open to a technical
walkthrough? I can share private demo access and a reproducible local benchmark.

Best,
the operator — viltsu.operator@gmail.com — GitHub: Sisuthros

---

## 5. Uncertain lead (low pressure — free Reliability Review foot-in-the-door)

**Subject:** A quick, no-cost look at your agent's reliability

Hi [Name],

Enjoyed the chat at the Expo. No pitch here — just an offer. If you're running
or building an AI agent that takes real external actions, I'll do a **free AI
Agent Reliability Review**: a short, focused look at where it could double-fire
an action, lose state on a crash, or act without a proper approval gate, plus
concrete suggestions.

FamilyClaw is the Rust-native runtime behind it (crash-safe dispatch, durable
replay, at-most-once dispatch under tested crash windows, fail-closed approval
gates), but the review stands on its own — you get value whether or not we work
together further.

If that's useful, reply and we'll find 20 minutes.

Best,
the operator — viltsu.operator@gmail.com

---

## Quick reference — offers & approval

| Offer | Scope | Price |
|-------|-------|-------|
| **A — AI Agent Reliability Review** | Focused review of one agent's reliability gaps | EUR 750–1500 (free version offered as foot-in-the-door) |
| **B — AI Agent Reliability Sprint** | 5-day pilot hardening ONE real agent workflow, scoped deliverable | EUR 1500–3500 (founding pilots; anchor at 3500, lower end = narrower pilot) |

**Approval gate:** These are drafts. Sending any message, publishing, or
agreeing to any engagement requires the operator's explicit approval. Do not send.

# FamilyClaw — Expo Brief Supplement

*Cyprus AI Expo · v1.2.0 · companion to [`EXPO_BRIEF.md`](./EXPO_BRIEF.md)*

---

This supplement adds only what the main brief lacks: a graduated set of
booth pitches (one-line / 15-second / 45-second), an explicit target-customer
description, and a call to action. For the problem statement, capabilities,
architecture diagram, crash-safe dispatch proof, demo commands, and honest
roadmap, see [`EXPO_BRIEF.md`](./EXPO_BRIEF.md).

> Note: `EXPO_BRIEF.md` already contains a one-line pitch (§1) and a
> thirty-second pitch (§2). The pitches below are the mandate's graduated
> ladder — one-line, 15-second, and 45-second — tuned for booth delivery.
> Where they overlap, they say the same true things in fewer words.

---

## Positioning (one sentence)

FamilyClaw is a **Rust-native reliability runtime for long-running AI agents
that take real external actions.** The wedge: *we make AI agents safe enough
to act in the real world.*

---

## Graduated pitches

### One-line pitch

> A Rust-native reliability runtime for long-running AI agents that take real
> external actions.

### 15-second pitch

> Long-running agents crash mid-task and retry actions that already spent
> money, sent an email, or wrote to an API. FamilyClaw is a Rust runtime that
> makes external actions crash-safe and at-most-once, gates high-impact
> actions behind content-bound approvals, and proves after the fact why every
> action happened. `unsafe` is forbidden across the workspace, and every
> behavior is backed by a local, deterministic test.

### 45-second pitch

> A single request/response demo never shows you how an agent fails in
> production. Real agents run for minutes, hours, or days — and they crash at
> the worst possible moment, right after they've taken an external action but
> before they've recorded that they did. Naive resume logic then re-runs that
> step, and a payment, email, or API write happens twice.
>
> FamilyClaw is a Rust-native runtime built for exactly that failure surface.
> It gives you crash-safe dispatch and durable deterministic replay from a
> journal — pure Rust, no external services, no re-executing closures — so a
> resumed agent reaches the same state. Under the specific crash windows we
> test, external dispatch is at-most-once: an action that already committed
> externally is never sent twice. High-impact actions pass through
> content-hash-bound approval gates that are TOCTOU-safe and fail closed, so
> an approval can only authorize the exact action it was granted for.
> Everything is auditable through turn-audit and approval endpoints plus
> Prometheus metrics. Provider failures are handled correctly — a dead key
> (401/403) rotates within the pool before the provider is cooled, while
> throttling (429) climbs a cooldown ladder — and the runtime fails closed on
> errors it must not retry.
>
> It's a 23-crate Rust workspace, `unsafe` forbidden workspace-wide, around
> 1,721 tests, with an 8/8 deterministic continuity scorecard. We can prove
> all of it live at the booth, on a laptop, with no keys and no network.

---

## Target customer

FamilyClaw is for **teams building AI agents that take real, irreversible
external actions** — agents that move money, send messages, write to
production APIs, or otherwise cause side effects that cannot be quietly undone.
The pain is sharpest for **long-running and autonomous agents** (workflows that
run for minutes to days, or unattended), where a crash, a retry, or a provider
failure at the wrong moment turns into a duplicated payment, a double-sent
email, or an action nobody can later explain. These teams already know that
demo-grade reliability is not production reliability; what they lack is a
runtime that makes the failure surface — crashes, duplicate dispatch, approval
enforcement, provider failover, auditability — *provably* handled rather than
hoped for. FamilyClaw is that reliability layer.

To be clear about maturity: FamilyClaw is a working, tested runtime with a
live, deterministic proof — **not** a product with production deployments,
customers, revenue, or certifications. The right first customer is one who
values a rigorously demonstrated reliability core and wants to pressure-test it
against one of their own agent workflows.

---

## Call to action

We are opening a small number of **founding pilots**. Two ways to start:

- **AI Agent Reliability Review — EUR 1,500** *(from EUR 750 for a narrower
  scope)*. A focused audit of one of your agent workflows against the failure
  surface FamilyClaw is built for: crash safety, duplicate external dispatch,
  approval enforcement, provider failover, and auditability. You get a written
  assessment of where the workflow is exposed.

- **AI Agent Reliability Sprint — EUR 3,500** *(from EUR 1,500 for founding
  pilots / a narrower pilot)*. A focused **5-day pilot** targeting **one** real
  agent workflow — we take a single workflow you care about and demonstrate,
  end to end, how the reliability layer behaves under the crash, retry, and
  provider-failure conditions that break agents in production.

Pricing is anchored at the top of each range for the full defined scope; the
lower figure is the concession for a deliberately narrower engagement.

**See it first.** The proof runs on a laptop, deterministically, with no keys
and no network:

```bash
# Flagship: two live agents on a bus — message delivery, memory recall,
# dream consolidation, and decay — all self-asserting.
cargo run -p familyclaw-agent --example two_agents_memory

# Crash proof: at-most-once external dispatch across tested crash windows.
cargo run -p familyclaw-agent --bin crash_replay -- full

# Full deterministic continuity scorecard (s1..s8).
cargo run -p familyclaw-bench --bin bench -- all
```

### Contact

- **The FamilyClaw Authors** — viltsu.operator@gmail.com
- **GitHub:** Sisuthros
- **Repository:** private — *private demo access on request.*

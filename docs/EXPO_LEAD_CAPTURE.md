# FamilyClaw — Expo Lead Capture

*A Rust-native reliability runtime for long-running AI agents that take real external actions.*
**We make AI agents safe enough to act in the real world.**

Use this template at the booth (paper or form). One card per conversation. Keep it to one page.

---

## What we qualify for

We are looking for people whose AI agents **take real external actions** (send emails, place orders, move money, call APIs, trigger deployments) and who **fear a specific failure** (duplicate side-effects on a crash, an unapproved action, a lost audit trail). If both are true, there is a pilot to talk about.

---

## Lead Card (blank template)

| Field | Answer |
|---|---|
| **Person** | |
| **Company** | |
| **Role** | |
| **Current AI workflow** (what the agent does) | |
| **External actions performed** (real-world actions the agent takes) | |
| **Biggest feared failure** (the specific thing that must not happen) | |
| **Current retry strategy** (how they handle crashes / provider errors today) | |
| **Approval requirements** (does a human sign off before actions? which ones?) | |
| **Buying authority** (can they approve spend? who signs off?) | |
| **Follow-up date** | |
| **Next action** (demo / call / send review offer / no-go) | |
| **Qualification score** (0–5, see rubric) | |

---

## Qualification score (0–5)

Award **1 point** for each "yes". Higher = better fit.

1. **Real external actions** — Does their agent take real external actions (not just chat / summarize)?
2. **Specific feared failure** — Do they fear a concrete failure (e.g. duplicate charge on a crash, an unapproved action, a missing audit trail)?
3. **Budget authority** — Do they have authority to approve a paid engagement, or a direct line to who does?
4. **Concrete pilot workflow** — Is there one specific, real agent workflow we could scope a pilot around?
5. **Available for follow-up** — Are they available and willing to take a follow-up call?

**Routing guide:**
- **4–5** → Pitch the **AI Agent Reliability Sprint** (5-day pilot for one real workflow, founding pilot). Book a follow-up call.
- **2–3** → Pitch the **AI Agent Reliability Review**. Send follow-up offer.
- **0–1** → Friendly, collect contact, no active follow-up.

---

## Worked example

| Field | Answer |
|---|---|
| **Person** | Maria K. |
| **Company** | (mid-size fintech ops tool) |
| **Role** | Head of Engineering |
| **Current AI workflow** | Agent reconciles invoices and issues refunds through their payments provider. |
| **External actions performed** | Calls the payments API to issue refunds; sends confirmation emails. |
| **Biggest feared failure** | A crash mid-run causes the same refund to be issued twice. |
| **Current retry strategy** | Ad-hoc: manual re-run after a crash, plus a homegrown "have we seen this ID" check they don't fully trust. |
| **Approval requirements** | Refunds over a threshold need a human to approve before they go out. |
| **Buying authority** | Can approve a small pilot budget directly; larger spend goes to the CTO. |
| **Follow-up date** | Thu next week, 14:00 |
| **Next action** | Book follow-up call + send Reliability Sprint one-pager. |
| **Qualification score** | **5/5** — real external actions (refunds), specific feared failure (double refund on crash), budget authority (pilot), concrete workflow (refund reconciliation), available for a call. |

*Why she scores 5:* her exact fear — **duplicate external dispatch under a crash window** — is what our crash-safe dispatch, durable deterministic replay, and duplicate-action prevention target, and her **human-approval-before-refund** requirement maps directly to our content-hash-bound approval gates. That is a concrete pilot.

---

## Talking points if they ask "what does it actually do?"

Only claim these — all real and tested:
- **Crash-safe dispatch** with **at-most-once external dispatch** under tested crash windows (no duplicate side-effects).
- **Durable deterministic replay** (journal + replay, pure Rust, no external services).
- **Content-hash-bound approval gates** — TOCTOU-safe, fail-closed; an action can't be swapped after approval.
- **Auditability** — audit and approvals endpoints, Prometheus metrics.
- **Provider/model failover** — key rotation and escalating cooldown ladders so it never hangs; fail-closed on non-retryable errors.
- **Deny-by-default WASM sandbox** (Wasmtime + Cranelift, fuel-metered); self-modification is impossible by construction.

**Live, no-keys, deterministic demos we can run on the spot:**
- Flagship: `cargo run -p familyclaw-agent --example two_agents_memory`
- Crash proof: `cargo run -p familyclaw-agent --bin crash_replay -- full`
- Scorecard: `cargo run -p familyclaw-bench --bin bench -- all`

**Honest boundaries (say these plainly):** no production deployments, no customers yet — this is a founding-pilot stage. The benchmark measures duplicate external side-effect dispatch under specific crash windows only, not throughput, latency, or model quality.

---

## The two offers

- **AI Agent Reliability Review** — EUR 750–1500. A review of one agent's reliability posture.
- **AI Agent Reliability Sprint** — EUR 1500–3500 (founding pilots). A focused **5-day pilot for ONE real agent workflow**.

*Pricing note (for the rep): anchor at the top of the range with a defined scope; the lower end is the concession for a narrower pilot.*

---

## Contact

**The FamilyClaw Authors** — viltsu.operator@gmail.com — GitHub: **Sisuthros**
Repo is private — *private demo access on request.*

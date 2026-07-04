# FamilyClaw — First 30 Days

**A Rust-native reliability runtime for long-running AI agents that take real external actions.**
Wedge: *We make AI agents safe enough to act in the real world.*

This is an execution roadmap built around **measurable outcomes**, not aspirations. Every milestone below has a defined owner, a proof of completion, an expected business impact, a failure condition, and a fallback. It starts at the Cyprus AI Expo and runs 30 days out. There are no production deployments, customers, revenue, or certifications yet — this plan is how we earn the first of each, honestly.

---

## What we can actually demonstrate

These are the only capabilities we will claim. All are real and tested against live code.

- **Crash-safe dispatch** with **durable deterministic replay** — a journal-and-replay design (pure Rust, no external services, no closure re-execution).
- **Duplicate-action prevention** — at-most-once external dispatch under tested crash windows.
- **Content-hash-bound approval gates** — TOCTOU-safe, fail-closed; a mismatch returns the `ApprovalPayloadMismatch` error variant rather than acting.
- **Auditability** — `/turns/audit` and `/approvals` endpoints, plus Prometheus metrics.
- **Persistent memory** with Ebbinghaus decay and a nightly dream consolidation cycle. The dream cycle reshapes the memory **set** (merges duplicates, grounds relative dates); **decay** is the separate mechanism that changes recall output. (We do **not** claim the dream cycle changes recall output.)
- **Provider/model failover** — on 401/403 rotate within the key pool before cooling the provider; on 429 an escalating cooldown ladder (60s / 5m / 25m / 1h); an auth ladder (5m / 30m / 2h / 6h); two-pass execution (healthy providers first, last-resort retries all so it never hangs); fail-closed on non-retryable `Parse` / `InvalidTool` results.
- **Wasmtime + Cranelift deny-by-default WASM sandbox** with fuel metering. Growth/self-modification is impossible by construction — `apply()` does not exist.

**Stack:** Rust workspace, 23 crates, v1.2.0, MIT-licensed, `unsafe` forbidden workspace-wide, ~1721 tests, a deterministic 8/8 scorecard (s1..s8), and a genuine pinned-LangGraph 1.2.6 crash-window benchmark.

**What the benchmark measures — and does not.** It measures duplicate external side-effect dispatch under specific crash windows *only*. It is **not** a throughput, latency, usability, or model-quality benchmark. We say this out loud in every conversation.

**Proof is local.** All demos and the scorecard run with no keys, no network, and are deterministic and self-asserting. Hosted CI may be billing-blocked; we claim local proof only, never a hosted-CI-passed claim.

### Demo commands (all verified present)

| Purpose | Command |
|---|---|
| Flagship (persistent memory, two agents) | `cargo run -p familyclaw-agent --example two_agents_memory` |
| Crash proof (duplicate-action prevention) | `cargo run -p familyclaw-agent --bin crash_replay -- full` |
| Scorecard (deterministic 8/8) | `cargo run -p familyclaw-bench --bin bench -- all` |

### Commercial offers

- **Offer A — AI Agent Reliability Review** — EUR 750–1500.
- **Offer B — AI Agent Reliability Sprint** — EUR 1500–3500 (founding pilots). A focused 5-day pilot for **one** real agent workflow.

Pricing is anchored at the **top** of each range with a defined scope. The lower end is the explicit concession for a narrower pilot — not the default.

### Contact

The FamilyClaw Authors — viltsu.operator@gmail.com — GitHub: Sisuthros.
The repository is private; **private demo access on request**.

---

## Owners (internal reference)

*Internal operations only — not public marketing.*

| Role | Owner | Scope |
|---|---|---|
| CEO / final authority | **the operator** | Customer relationships, all approvals |
| Strategy / product synthesis | **agent_alpha** | Prioritization, decision framing |
| Engineering lead | **Claude (assistant)** | Implementation, technical audit, delivery |
| Market intelligence | **Grok** | Competitor analysis, adversarial review |
| Lead research | **agent_epsilon** | Personalized demos, outreach prep |
| QA | **agent_gamma** | Claim verification, counter-evidence |

Agents may autonomously research, analyze, draft, code in branches, test, prepare demos, and prepare outreach drafts. **the operator approval is required** before: sending messages, publishing, merging to main, deployments, spending money, signing agreements, accessing customer production systems, handling customer secrets, or any destructive action.

---

## EXPO DAY — Cyprus AI Expo

Goal: leave the floor with a real, qualified top of funnel. No selling fantasy — the wedge is one sentence and one live demo.

### Milestone E1 — 10 qualified conversations
- **Owner:** the operator (floor), agent_epsilon (qualification notes)
- **Proof of completion:** 10 named contacts captured with company, the agent workflow they run, and whether that workflow takes real external actions.
- **Business impact:** A real top of funnel; without it nothing downstream exists.
- **Failure condition:** Fewer than 10 qualified by end of day, or contacts with no real agent workflow.
- **Fallback:** Requalify against the wedge ("does your agent take real external actions?"); drop unqualified names and prioritize the 5 strongest for follow-up rather than padding the count.

### Milestone E2 — 3 live demos
- **Owner:** the operator / agent_epsilon
- **Proof of completion:** 3 contacts saw one of the three demo commands run to completion — flagship, crash proof, or scorecard — deterministic, no keys, no network.
- **Business impact:** Turns a claim ("safe enough to act") into a witnessed fact; the crash-proof demo is the differentiator.
- **Failure condition:** A demo fails to run on the expo machine, or is misrepresented as a throughput/quality benchmark.
- **Fallback:** Fall back to the pre-recorded demo (see F2) and re-run live later; correct any benchmark overclaim on the spot.

### Milestone E3 — 2 booked follow-up calls
- **Owner:** the operator
- **Proof of completion:** 2 discovery calls on the calendar with date, time, and attendee.
- **Business impact:** Converts floor interest into a scheduled sales motion.
- **Failure condition:** Interest expressed but nothing booked.
- **Fallback:** Send calendar links within the first 72 hours (F1) and treat booking as a follow-up KPI instead of an expo KPI.

### Milestone E4 — 1 serious pilot candidate
- **Owner:** the operator (decision), agent_alpha (fit framing)
- **Proof of completion:** 1 contact with a named agent workflow that takes real external actions, a stated reliability pain, and willingness to discuss a paid pilot.
- **Business impact:** The single most valuable expo outcome — the seed of the first paid pilot.
- **Failure condition:** No candidate meets all three criteria (real actions + stated pain + pilot-willing).
- **Fallback:** Widen to Offer A (Review) candidates; a Review is a lower-commitment entry that can mature into a Sprint.

---

## FIRST 72 HOURS

Goal: capitalize on expo memory before it fades. Everything here is producible without spending money, publishing publicly, or sending anything without the operator's approval.

### Milestone H1 — Personalized follow-ups sent
- **Owner:** agent_epsilon (drafts), the operator (approval + send)
- **Proof of completion:** One tailored message per qualified contact, each referencing their specific workflow, sent after the operator's approval.
- **Business impact:** Personalization is the difference between a follow-up and a booked call.
- **Failure condition:** Generic copy, or messages drafted but not approved/sent within 72 hours.
- **Fallback:** Prioritize the E4 candidate and E3 booked contacts first; send the remainder in a second batch.

### Milestone H2 — One landing-page-ready offer
- **Owner:** agent_alpha (offer framing), assistant (technical claims), agent_gamma (claim verification)
- **Proof of completion:** A single-page description of Offer A and Offer B with scope, price anchoring at the top of the range, the exact wedge sentence, and only capabilities from the verified list. Ready to publish; **publishing requires the operator's approval.**
- **Business impact:** Gives every follow-up a concrete thing to say yes to.
- **Failure condition:** Any unverified claim, invented metric, or hosted-CI-passed claim slips in.
- **Fallback:** agent_gamma runs a counter-evidence pass and strips anything not backed by live code before it goes near a customer.

### Milestone H3 — One demo recording
- **Owner:** assistant
- **Proof of completion:** A recording of `crash_replay -- full` (or the flagship example) running to completion, with narration stating exactly what the crash-window benchmark measures and does not measure.
- **Business impact:** A reusable asset for follow-ups and for any demo that can't run live.
- **Failure condition:** Recording overstates scope or implies throughput/quality claims.
- **Fallback:** Re-record with corrected narration; agent_gamma reviews before it is shared.

### Milestone H4 — Discovery-call structure
- **Owner:** agent_alpha
- **Proof of completion:** A written call structure: identify the agent workflow, confirm it takes real external actions, surface the reliability pain, map it to a capability, propose Offer A or B.
- **Business impact:** Makes discovery calls repeatable instead of improvised.
- **Failure condition:** No structure, so calls drift and don't qualify.
- **Fallback:** Use a minimal three-question version (workflow / real actions / pain) until the full structure is written.

### Milestone H5 — Lead qualification done
- **Owner:** agent_epsilon, agent_gamma
- **Proof of completion:** Every expo contact tagged: real external actions (yes/no), pain severity, offer fit (A/B/none).
- **Business impact:** Focuses limited time on the few leads that can actually pay.
- **Failure condition:** Leads carried forward without a real-actions determination.
- **Fallback:** Default an untagged lead to "none" until qualified — never assume fit.

### Milestone H6 — One concrete pilot proposal
- **Owner:** agent_alpha (scope), the operator (approval), assistant (feasibility)
- **Proof of completion:** A written Offer B proposal for the E4 candidate: one named workflow, 5-day scope, deliverables, price anchored at the top of the range.
- **Business impact:** The first document that could turn into the first euro.
- **Failure condition:** Scope covers more than one workflow, or promises anything outside the verified capabilities.
- **Fallback:** Narrow scope and drop to the lower end of the range as the explicit concession for a smaller pilot.

---

## FIRST 7 DAYS

Goal: convert follow-ups into structured conversations and one real proposal, and start learning from actual customers.

### Milestone D1 — Discovery calls held
- **Owner:** the operator (calls), agent_alpha (structure), agent_epsilon (notes)
- **Proof of completion:** The E3 booked calls held using the H4 structure, each with written notes on workflow, real-actions status, and pain.
- **Business impact:** Direct evidence of which reliability pains customers will pay to remove.
- **Failure condition:** Calls slip or happen without capturing the qualifying facts.
- **Fallback:** Reschedule once; if a lead goes cold, reallocate the time to the next-strongest qualified contact.

### Milestone D2 — First paid proposal
- **Owner:** the operator (send + approval), agent_alpha (scope), agent_gamma (claim check)
- **Proof of completion:** At least one Offer A or Offer B proposal sent to a real prospect, priced at the top of its range with defined scope, after the operator's approval.
- **Business impact:** The first genuine commercial ask.
- **Failure condition:** No proposal sent in 7 days, or a proposal containing an unverified claim.
- **Fallback:** Lead with Offer A (Review, EUR 750–1500) as the lower-commitment entry point if a Sprint feels premature to the prospect.

### Milestone D3 — Customer-problem repository started
- **Owner:** Grok (structure), agent_epsilon + agent_gamma (entries)
- **Proof of completion:** A living document capturing each customer's stated reliability problem, current workaround, and which FamilyClaw capability maps to it.
- **Business impact:** Builds the evidence base that drives honest roadmap decisions.
- **Failure condition:** Problems collected as anecdotes with no mapping to real capabilities.
- **Fallback:** Grok runs an adversarial pass to separate "real, recurring pain" from one-off comments before anything influences the roadmap.

### Milestone D4 — Roadmap updated from real evidence
- **Owner:** agent_alpha (synthesis), assistant (feasibility), the operator (approval)
- **Proof of completion:** A roadmap revision that cites specific entries from the D3 repository as justification for each priority change.
- **Business impact:** Ensures build effort tracks paying-customer reality, not internal guesses.
- **Failure condition:** Roadmap changed on opinion, or promises capabilities that don't exist yet.
- **Fallback:** Defer any change not backed by at least one repository entry until more evidence exists.

---

## FIRST 30 DAYS

Goal: land the first paid pilot, deliver it honestly, and turn the work into reusable leverage and a higher next ask. No revenue projections beyond what a signed pilot actually is.

### Milestone M1 — First paid pilot
- **Owner:** the operator (agreement + payment approval), assistant (delivery), agent_gamma (verification)
- **Proof of completion:** A signed Offer B (Reliability Sprint) for one real agent workflow, with agreed scope, delivered over 5 focused days. Signing the agreement and accepting payment are the operator-approved actions.
- **Business impact:** The first real customer and the first revenue — the transition from claim to business.
- **Failure condition:** No pilot signed in 30 days, or a pilot scoped beyond one workflow / beyond verified capabilities.
- **Fallback:** Convert the strongest Offer A (Review) into a scoped Sprint; if no Sprint closes, deliver a paid Review as the beachhead.

### Milestone M2 — Anonymized case-study permission
- **Owner:** the operator (request + approval), agent_epsilon (draft)
- **Proof of completion:** Written permission from the pilot customer to publish an anonymized account of the problem, what was delivered, and the measured result. Publishing requires the operator's approval.
- **Business impact:** The first piece of honest social proof for future prospects.
- **Failure condition:** Case study drafted without permission, or embellished beyond what was delivered.
- **Fallback:** If permission is withheld, retain the internal-only account as sales-call reference and ask again after value is proven.

### Milestone M3 — Reusable components moved into FamilyClaw
- **Owner:** assistant (implementation), agent_gamma (test coverage)
- **Proof of completion:** Any generalizable work from the pilot merged into the FamilyClaw workspace with tests, keeping `unsafe` forbidden and the scorecard at 8/8. Merging to main is a the operator-approved action.
- **Business impact:** Each pilot makes the next one cheaper and faster to deliver.
- **Failure condition:** Pilot code left as one-off, or a merge that breaks the scorecard or introduces `unsafe`.
- **Fallback:** Land the work on a branch behind approval; do not merge to main until the scorecard is green and the operator has approved.

### Milestone M4 — Next proposal at a higher price
- **Owner:** agent_alpha (scope + pricing), the operator (send + approval)
- **Proof of completion:** The next proposal sent priced above the first, justified by the delivered pilot and the M3 reusable components.
- **Business impact:** Establishes an upward pricing trajectory grounded in proven delivery, within the stated ranges.
- **Failure condition:** Repricing without delivery evidence, or exceeding the defined offer ranges.
- **Fallback:** Hold price and expand scope instead if the market signals the anchor is too high for a second customer.

### Milestone M5 — Recurring support opportunity identified
- **Owner:** the operator (relationship), agent_alpha (offer shape), Grok (market check)
- **Proof of completion:** A documented, named opportunity for ongoing work with the pilot customer (e.g. continued reliability support), with a clear boundary that production-system access and customer secrets remain the operator-approved.
- **Business impact:** The first path from one-off pilots toward repeatable, ongoing revenue.
- **Failure condition:** Assuming recurring demand exists with no signal from the customer.
- **Fallback:** Keep the relationship warm with periodic honest check-ins; revisit only when the customer expresses a concrete ongoing need.

---

## Guardrails for the whole 30 days

- **Claim only what the code proves.** agent_gamma verifies every customer-facing claim against live code. No invented numbers, no customers we don't have, no certifications, no enterprise claims, no hosted-CI-passed claim, no "27 crates" (it is 23).
- **Say what the benchmark is not.** Every time the crash-window benchmark comes up, state that it measures duplicate external side-effect dispatch under specific crash windows only — not throughput, latency, usability, or model quality.
- **the operator approves the real-world actions.** Sending, publishing, merging to main, deploying, spending, signing, and any access to customer production systems or secrets are gated on the operator's approval — exactly the discipline the product itself enforces.
- **Truth over spectacle.** If a milestone slips, we record it and take the fallback. We do not paper over it.

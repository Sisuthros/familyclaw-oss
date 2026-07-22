# FamilyClaw — One-Pager

**A Rust-native reliability runtime for AI agents that take real external actions.**

*Draft for buyer evaluation. No production deployments, customers, or
certifications yet — early clients are founding pilots. Every capability
claim below is tested; see `docs/commercial/QUICKSTART.md` for a real,
timestamped verification run, and `docs/COMMERCIAL_OFFER.md` for full offer
detail.*

---

## The problem

AI agents are starting to take **real external actions**: sending money,
issuing refunds, provisioning cloud infrastructure, writing to a customer's
production systems, running data migrations. The moment an agent does that,
a new failure class opens up that ordinary "durable" agent frameworks don't
close: a crash, retry, or race condition after the external action has
already fired can make it fire **again** — a duplicate charge, a duplicate
refund, a double-provisioned resource, a re-applied migration step.

Checkpointing solves "did we lose our place." It does not solve "did we do
the thing twice." That second gap is where a crash turns into an incident,
a support ticket, or a compliance problem.

## What FamilyClaw does

FamilyClaw is a Rust workspace (23 crates, MIT-licensed, `unsafe` forbidden
workspace-wide) built specifically to make that failure mode impossible **by
construction**, not just unlikely:

- **At-most-once external side-effect dispatch under crash.** An
  idempotency-keyed intent → effect → committed-outbox model. Benchmarked
  head-to-head against a leading durable-execution framework's strongest
  durability setting on one narrow, honest metric — how many money-touching
  side effects re-execute after a process crash. FamilyClaw: zero, at every
  tested crash point.
- **Content-hash-bound approval gates.** Fail-closed and TOCTOU-safe — if
  the payload changes after a human approves it, dispatch is refused rather
  than silently acting on the new content.
- **Durable, deterministic crash replay.** A journal-and-replay model that
  reconstructs state after a crash without re-executing side-effecting
  code, in pure Rust, with no required external services.
- **Full auditability.** HTTP audit and approval endpoints, Prometheus
  metrics, out of the box.
- **Provider/model failover** with a tested rotation, cooldown, and
  fail-closed ladder, so a single flaky upstream provider doesn't take the
  agent down.
- **A sandboxed skill layer** (WASM, deny-by-default, fuel-metered) where
  self-modification is impossible by construction — there is no code path
  that lets the growth layer rewrite itself.

**What is proven, honestly:** an 8/8 deterministic scorecard covering
crash-replay, memory durability, emotional-state isolation, provenance
gating, and consolidation — reproducible in one command, no API keys
required. A real crash-window benchmark against a pinned competitor
version. Proof to date is local and self-run; there is no hosted-CI-passed
claim and no production-scale claim.

## Deployment model

- **Self-hosted.** Ships as a Rust workspace / binary; a Docker deployment
  path is documented. No SaaS dependency, no data leaves your
  infrastructure unless you wire an external LLM provider yourself.
- **Bring your own model provider.** FamilyClaw orchestrates and hardens
  the dispatch layer; it does not lock you into a specific LLM vendor.
- **Runs with zero configuration for evaluation** (a channel-less demo
  mode), and against a real chat channel (Discord/Telegram documented) or a
  custom integration for production use.
- **Private repository.** Demo access granted on request during
  evaluation; no public listing.

## Commercial offers

Two engagements, EUR 1500–3500, scoped narrowly on purpose — one real
workflow, not a platform migration:

| | **Reliability Review** | **Reliability Sprint** |
|---|---|---|
| **Price** | EUR 750–1500 | EUR 1500–3500 |
| **Timeline** | 2–3 days | 5 days |
| **What you get** | Architecture review of one agent workflow's action-dispatch and retry logic; a failure-mode map; a prioritized remediation plan. No code changes. | One real workflow hardened end to end: crash/retry-safe dispatch, at-most-once external actions, an approval gate, audit evidence, a provider-failover review — delivered as a **working proof of concept** against your test/staging environment. |
| **Requires from you** | One workflow description + redacted code/log access + one technical contact. | A defined workflow, a test/staging target, code access, a technical contact for the week. |
| **Does not include** | Implementation, deployment, production access. | Production cutover, additional workflows, customer secrets/production access without separate scope. |

Pricing is anchored at the top of each range for the fully scoped
engagement; the lower end is the concession for a narrower pilot (a smaller
workflow, tighter scope). Price moves by removing scope, not by discounting
the full scope.

**Upgrade path:** Review → Sprint → ongoing support (retainer, additional
workflows, production-readiness work), each scoped separately once the
prior stage's findings are in hand.

## Support terms (draft)

- **During an engagement:** direct technical contact for the engagement's
  duration (async + one scheduled working session per stage).
- **After a Sprint:** a documented handover (what was built, what is
  proven, recommended next steps) — no implicit ongoing support beyond
  that unless a retainer is separately agreed.
- **No production on-call / SLA included** in Review or Sprint pricing;
  that is retainer-scope work, quoted separately once a Sprint has shipped.
- **No access to your production systems or customer secrets** is required
  for either offer unless explicitly and separately approved by you in
  writing.

## Who this is for

You already run, or are about to ship, an agent that takes real external
actions and you are not confident it survives a crash without doing
something twice. You have one concrete workflow you can point at (not "our
whole platform"), and someone who can answer technical questions about how
it currently dispatches actions and handles retries.

## Who this is not for

Your agent only reads, summarizes, and drafts — no external side effects.
A crash costs you a re-run and nothing more. You don't need this yet; stay
where you are.

---

*FamilyClaw · Rust workspace · 23 crates · MIT · `unsafe` forbidden
workspace-wide · deterministic 8/8 continuity scorecard, reproducible with
no API keys. No production deployments, customers, or certifications yet —
early clients are founding pilots. Contact method to be added by the repo
maintainer before this document is shared externally.*

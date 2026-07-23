# FamilyClaw — 15-Minute Evaluator Demo Script

**Audience:** a prospective buyer's technical contact, in a live call or a
self-serve evaluation, deciding whether to book a Reliability Review or
Sprint (`docs/commercial/ONE_PAGER.md`).

**Goal:** in 15 minutes, with zero API keys and zero configuration, the
evaluator sees (a) the product build cleanly, (b) the specific failure mode
FamilyClaw closes — duplicate external side effects after a crash — proven
live, not asserted, and (c) enough of the memory/continuity story to
understand what "family of persistent agents" means.

Every command here was run and its real output captured in
`docs/commercial/QUICKSTART.md` on 2026-07-22/23 — this script is the
live-demo cut of that verification run, reordered for narrative impact
rather than build order.

**Prerequisite (do this before the call, not during it):** clone the repo
and run `cargo build --workspace` once, so the ~1-minute cold-compile is not
eating the evaluator's 15 minutes. Everything after that runs in seconds.

---

## Minute 0–1: Frame the problem out loud

Say this, don't read it verbatim, but hit these beats:

> "If your agent only reads and answers, a crash costs you a re-run — not
> interesting. The moment it *acts* — sends money, provisions a resource,
> issues a refund — a crash during that action becomes a duplicate. That's
> the failure I'm going to show you closed, live, in about ten minutes.
> Nothing you're about to see needs an API key — it's all deterministic."

---

## Minute 1–4: The crash-replay proof (the core wedge)

Run it live:

```bash
cargo run -p familyclaw-agent --bin crash_replay -- full
```

Narrate while it runs (it takes under a second):

- **Phase 1** writes a memory to a real on-disk journal and store —
  this simulates the state right before a crash.
- **Phase 2 is a fresh OS process** — not a resumed one — reopening
  *only* what's on disk.

Point at the real output:

```
🎯 CRITICAL TEST: Memory recall after process restart
   Hits:  1
   ✅ SUCCESS: Memory SURVIVED process boundary!
   Content: This is a critical memory that must survive a crash!
   Retention: 1.00
```

**The line to land:** "That second process never talked to the first one.
It read a journal off disk and reconstructed exact state — including which
steps already completed — so a resumed agent doesn't re-fire an action that
already went out." Point them at `docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md`
for the deeper crash-window matrix if they want engineering-level depth
after the call.

---

## Minute 4–8: The scorecard — breadth in one command

```bash
cargo run -p familyclaw-bench --bin bench -- all
```

While it runs: "This is the same idea, but as a regression-tested suite —
8 independent scenarios, each with a numeric PASS/FAIL, not a vibe." Scroll
to the bottom:

```
benchmark complete: ALL PASSED
```

Optionally scroll up to one scenario evaluators tend to ask about —
`s7_provenance_gate` (blocks poisoned/untrusted memory from being admitted)
or `s1_crash_matrix` (the crash scenario from Minute 1–4, formalized) — and
show the actual metric table, not just the PASS line. This is the moment to
say: "Every one of these regenerates from source — nothing here is a
screenshot or a claim, it's `cargo run` and read the number."

---

## Minute 8–12: The "family" story — memory, emotion, decay

```bash
cargo run -p familyclaw-agent --example two_agents_memory
```

This is the most narratively interesting demo — two named agents (Alice,
Bob) on a shared bus. Walk through the live proof summary at the end:

```
(2) real message delivery ................ Alice → bus → Bob stored & recalled it
(3) real emotion propagation ............. Bob's joy 0 → 18 over the bus
(4) dream consolidation effect ........... Bob's active memories 4 → 3, dates absolutized 1
(5) decay / protected anchor effect ...... same query, day1 ≠ day8 top memory
(6) identity anchor survived decay ....... ProtectedCore retention stayed 1.00
```

**The line to land:** "This isn't a chatbot with a vector DB bolted on —
memories decay on a real Ebbinghaus curve, a nightly 'dream' cycle merges
duplicates and grounds relative dates, and there's a protected-anchor
mechanism so identity-critical memories don't decay away with the rest. If
your use case is a single stateless task-runner, you don't need this part —
it's here because the target buyer runs *persistent* agents, not one-shot
chat sessions."

---

## Minute 12–14: The honest boundary — show the failure mode, not just the win

This is the credibility moment. Run:

```bash
cargo run -p familyclaw-gateway -- doctor
```

Let it fail on screen:

```
[MISSING] env       TELEGRAM_BOT_TOKEN
Error: InvalidInput("doctor: one or more checks failed")
```

**Say:** "I'm showing you this on purpose. With zero config, the default
channel needs a real bot token, and the tool refuses to start instead of
silently running broken. That's the same 'fail closed, don't guess'
philosophy the crash-safety guarantee is built on — I'd rather show you a
refusal than a fake green checkmark." Then flip it:

```bash
FAMILYCLAW_CHANNEL_KIND=none cargo run -p familyclaw-gateway -- doctor
```

```
doctor: ok
```

"One environment variable, and it's a clean pre-flight pass — that's the
evaluation path, not the production path."

Optional competitor beat (30–60s):

```bash
python bench-competitors/openclaw/crash_harness.py cycle --crash-point before_write --workdir /tmp/oc-bw
# Expect side_effect_overcount: 1 — FamilyClaw stays 0 (`cargo run -p familyclaw-bench -- s1`)
```

Say: shaped OpenClaw/Hermes MEMORY.md models under a real process kill —
not a claim we audited their latest binary. LangGraph is the pinned live
framework (`bench-competitors/langgraph/`).

---

## Minute 14–15: Close — what happens next

Say:

> "Everything I just showed you is exactly reproducible — no keys, no
> setup, the commands are in `docs/commercial/QUICKSTART.md` with real
> output already pasted in, timestamped, so you don't have to trust me,
> you can run it yourself tonight. The two offers on the one-pager are: a
> Review, where I map your actual workflow's failure modes against what you
> just saw, or a Sprint, where I harden one real workflow of yours to this
> standard against your test/staging environment and hand you a working
> proof of concept in five days. Which of your agent workflows worries you
> most right now — the one that's closest to touching money or production
> systems?"

Then stop talking. Let them answer — that answer is the workflow that
becomes the Review or Sprint scope.

---

## If something goes wrong live

- **Build is slow / not pre-warmed:** you skipped the prerequisite. Fall
  back to reading `docs/commercial/QUICKSTART.md` output on screen instead
  of running live — it's real output from an actual run, not a mockup.
- **`doctor` behaves differently than shown:** the exact output depends on
  the local `familyclaw.toml` / env state of the machine you're demoing on.
  Use a clean checkout with no config file for this script to match exactly.
- **An evaluator asks about the LangGraph benchmark numbers:** those live
  in `bench-competitors/langgraph/RESULTS.md` — mention it is a pinned,
  reproducible comparison, but note (honestly) that it was not re-run in
  the same session as this script's other demos and offer to re-run it
  live if they want to see it (it takes a Python venv + ~5 minutes).

---

*Companion documents: `docs/commercial/ONE_PAGER.md` (offer + pricing),
`docs/commercial/QUICKSTART.md` (full verified command log with real
output).*

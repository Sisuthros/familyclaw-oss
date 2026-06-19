# Who FamilyClaw is for

> **One sentence:** FamilyClaw is a Rust agent runtime where an agent's *external side
> effects survive a crash at most once* — a mid-action SIGKILL never double-fires a
> charge, a message, or a migration.

This doc is the go-to-market companion to the engineering. It answers one question
honestly: **should you use this?**

---

## Should you use this?

**Probably not, if** your agent only *reads and summarizes* — retrieves context,
answers, drafts. A mid-run crash costs you a re-run, nothing more. Stay in Python;
every model SDK, eval tool, and integration is already there. We do not win on being
"Rust," and we will not pretend otherwise.

**Yes, if** your agent *mutates the world and a crash costs real money or real trust* —
it issues refunds, sends messages, moves cloud resources, runs migrations. The thing
that breaks you isn't losing state (checkpointers handle that); it's a node whose
external side effect already fired, then re-runs on resume and fires **again**. That is
the gap FamilyClaw closes, and the gap most "durable" agent frameworks leave open
(measured: see `bench-competitors/langgraph/RESULTS.md`).

**The honest boundary of the guarantee:** at-most-once *dispatch* of external side
effects under crash — never twice; a crash in the narrow window after the effect fires
but before its durable record is written fails *closed* (zero or one execution, then
recovery) rather than re-firing. It is duplicate-prevention under crash, **not** a
promise of universal "exactly-once completion." Durable *state* replay (which good
checkpointers also provide) is table stakes, not our wedge.

---

## Three people this is for

1. **The solo dev burned by a re-run migration.** Runs an overnight schema/data
   migration agent. A crash mid-migration on a checkpoint-only framework re-applied a
   step on resume; now the data is wrong. For them, "checkpoint" was never "exactly
   once" — they need at-most-once dispatch on the migration step.

2. **The infra team with an autonomous cost-cleanup agent.** It deletes idle cloud
   resources and tears down stacks — irreversible, money-touching mutations running
   unattended. A double-fired teardown after a crash is an outage. They need the side
   effect to fire at most once, and to fail closed if uncertain.

3. **The fintech tinkerer whose agent issues refunds.** A mid-action crash that
   re-issues a refund is a duplicate payout and a support ticket. They cannot afford a
   double-charge or a double-refund; fail-closed-then-reconcile is exactly right.

These are people for whom *checkpoint ≠ exactly-once*, and for whom a benchmark they
can run themselves in one command is worth more than a feature list.

---

## The adoption gate (the metric that matters)

Stars on a repo are vanity. The real signal is one number:

> **At least one external person runs `familyclaw serve` in their own repo and reports
> it.**

Until that happens, FamilyClaw is a strong engine without a user. After it happens, we
have a wedge with a witness. Everything else — Show HN, r/rust, sponsors — is in service
of that single gate, not a substitute for it.

---

## How to prove the claim yourself

```
git clone <repo> && cd familyclaw
cargo run -p familyclaw-bench -- compare        # FamilyClaw vs a shaped baseline
# and the cross-process at-most-once proof:
cargo test -p familyclaw-actions --test redteam_dispatch_exactly_once
```

A skeptic watches FamilyClaw survive a real SIGKILL mid-side-effect with the external
side-effect counter staying at exactly 1, while the comparison baseline re-fires. The
real-competitor (LangGraph) comparison is in `bench-competitors/langgraph/` — run it and
check the numbers; the exact config is pinned so you can reproduce it, not just trust it.

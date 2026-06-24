# Crash-Safe Dispatch Case Study

## Why checkpointing is not enough

State checkpointing is necessary for long-running agents, but it is not sufficient once an agent performs external work.

A checkpoint can remember that an agent was about to do something. It does not, by itself, prove that an external side effect will not fire again after a crash and restart.

Examples of external side effects:

- send a message
- create an issue
- call a webhook
- trigger a deployment
- submit an approval result
- write to an external system

For real agents, the dangerous question is not only:

> Can the agent remember where it was?

The dangerous question is:

> Can the agent recover without doing the external action twice?

## The S1 Crash Matrix

FamilyClaw’s continuity benchmark includes the S1 Crash Matrix.

The test crashes the system at different points around external side-effect dispatch and then restarts it. After restart, it checks whether the external side effect was duplicated and whether the workflow resumes correctly.

## Result

| Metric | FamilyClaw | Baseline |
|--------|------------|----------|
| Result | PASS | FAIL |
| Side-effect overcount | 0 | 17 |
| Resume correctness | 1.0 | 0.0 |

Plain English:

> FamilyClaw did not repeat the external side effect after crash/restart. The baseline did.

## The reliability model

FamilyClaw uses an idempotency-keyed dispatch model:

```text
intent -> effect -> committed
```

The durable journal records enough state to distinguish these cases:

1. The effect was already committed.
2. The effect was only intended but not safely committed.
3. The system crashed in a narrow uncertain window.

The safe behavior is intentionally conservative:

- committed dispatches replay as committed values
- duplicate dispatch is prevented
- intent-only crash fails closed
- recovery may require explicit handling

This is an **at-most-once dispatch** guarantee under crash. It is not a universal exactly-once completion guarantee.

## Honest baseline note

The in-repo comparison uses a shaped baseline to model the failure mode of checkpoint/file-memory style systems that can remember state but still re-run external work after restart.

The broader architectural lesson applies to LangGraph-style systems:

> checkpointing state is not the same as crash-safe external action dispatch.

FamilyClaw should not claim to have tested every external framework’s internal implementation. The claim is narrower and stronger:

> FamilyClaw proves a reliability layer that checkpointing alone does not provide.

## The one-line distinction

```text
Checkpointing remembers the scene.
FamilyClaw guards the trigger.
```

## Why this matters

Toy agents can survive with memory and prompts. Useful agents need operational guarantees.

Before an agent can safely send messages, create tickets, trigger workflows, call webhooks, or run approved actions, it must have a durable dispatch model that prevents accidental duplicate external actions after crash/restart.

FamilyClaw Phase 1 lands that reliability wedge.
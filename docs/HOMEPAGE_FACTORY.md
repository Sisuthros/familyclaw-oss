# Homepage Factory — verified multi-agent collaboration

The **Homepage Factory** is an end-to-end demonstration that FamilyClaw's bridge
layer can run a real multi-agent workflow with **verified contracts** today — and
that the *same* plan will run on a real LLM tomorrow with **zero changes to the
orchestrator**.

It lives as:

- **Data + test:** [`crates/familyclaw-bridge/tests/homepage_factory.rs`](../crates/familyclaw-bridge/tests/homepage_factory.rs)
- **Engine:** the bridge crate's `orchestrator`, `contract`, and `executor` modules.

## The scenario

Three agents collaborate to build a product homepage:

| Agent  | Role (`AgentRole`) | Capability        | Job                          |
|--------|--------------------|-------------------|------------------------------|
| agent_epsilon | `Strategy`         | `homepage_design` | Designs the homepage         |
| agent_beta | `Scout`            | `review`          | Reviews / approves the design |
| agent_gamma | `Executor`         | `deploy`          | Ships it to production       |

The work is modeled as a **DAG** (`OrchestrationPlan`):

```
design ──▶ review ──▶ deploy
```

- `design` requires capability `homepage_design`, has no dependencies, and
  **carries a contract**: its deliverable must match the `HomepageDesign` output
  schema and satisfy the postconditions before the node may reach `Done`.
- `review` depends on `design`.
- `deploy` requires capability `deploy`, depends on `review`, and carries its own
  output contract (`result: object`).

## The verified contract

`design`'s capability declares a typed promise — this is what separates "the
agent said it was done" from "the work provably meets the spec":

```
homepage_design:
  input  (BrandBrief):   { brand: Str, audience: Str }
  output (HomepageDesign): { headline: Str, sections: Arr, cta: Str }
  postconditions:
    - non_empty(headline)
    - min_len(sections, 1)
```

When the orchestrator finishes a node that carries a capability, it runs the
deliverable through `ContractBoard::fulfill`, which checks the **output schema**
and **every postcondition**. A breach moves the contract to `Failed` and the node
never reaches `Done` — the DAG halts at the contract boundary instead of silently
propagating a malformed result downstream.

## The two proofs (tests)

### 1. `homepage_factory_runs_end_to_end` — the happy path

Runs the plan via `Orchestrator::run_with(&plan, now, &MockTurnExecutor::default())`
and asserts:

- All **3 nodes reach `Done` in dependency order** (`design` → `review` → `deploy`).
- The `design` deliverable **passes the `homepage_design` contract** — output
  schema + postconditions — so the contract is `Fulfilled` (proven explicitly via
  a `ContractBoard` linked to the design task with `Contract.link = Some(task_id)`).
- The `orchestration.step_assigned` events **fire in order** (`design`, `review`,
  `deploy`).

### 2. `malformed_design_halts_factory_at_contract_boundary` — the reliability guarantee

Runs the *same plan* with `MockTurnExecutor::failing()`, which produces a
`HomepageDesign` that is **missing `headline` and has empty `sections`**, and
asserts:

- The `design` contract goes **`Failed` at `fulfill()`** (output schema violation).
- The `design` node **does not reach `Done`** (its task halts in `Active`).
- `review` and `deploy` are **never assigned** — the DAG stops at the breach.
- Exactly one `orchestration.step_failed` event fires for `design`, and only
  `design` was ever assigned.

This is the core reliability claim: a malformed deliverable is **caught at the
contract boundary, not silently propagated**.

## Mock today, live tomorrow — zero orchestrator change

The orchestrator depends only on the `TurnExecutor` seam, never on a concrete
executor or LLM:

```rust
#[async_trait]
pub trait TurnExecutor: Send + Sync {
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable>;
}
```

- **Today (consumer side, this crate):** `MockTurnExecutor` is hermetic and
  deterministic — no clock, no network, no randomness. Its payload is derived
  purely from the turn's input and assignee, so the factory test is reproducible.
- **Tomorrow (producer side):** the producer team implements `LiveTurnExecutor` in
  `familyclaw-agent` behind the **same** `TurnExecutor` trait, wiring a real free
  LLM (and transport). Swapping `MockTurnExecutor` for `LiveTurnExecutor` in the
  `run_with(...)` call makes the identical plan run against a live model — the
  orchestrator, plan, contracts, and DAG semantics are untouched:

```rust
// today
let report = orch.run_with(&plan, now, &MockTurnExecutor::default()).await?;

// tomorrow — same plan, same orchestrator, only the executor changes
let report = orch.run_with(&plan, now, &live_executor).await?;
```

The contract guarantees travel with the plan, not the executor: whether the
`HomepageDesign` came from a deterministic mock or a real LLM, it must pass the
same output schema + postconditions before `design` can reach `Done`.

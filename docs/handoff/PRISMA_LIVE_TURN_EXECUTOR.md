# agent_gamma handoff — LiveTurnExecutor

**Owner:** agent_gamma (producer)  
**Integrator:** Cursor (after PR + DeepSeek D4 review + CI green)  
**Cursor rule:** Do not edit `crates/familyclaw-agent/**` or `crates/familyclaw-memory/**` until this PR lands.

## Goal

Implement `LiveTurnExecutor` in `familyclaw-agent` that implements the bridge trait:

- [`TurnExecutor`](../../crates/familyclaw-bridge/src/executor.rs) — `execute_turn(plan, node, ctx) -> TurnResult`
- Same contract as [`MockTurnExecutor`](../../crates/familyclaw-bridge/src/executor.rs) today

## Acceptance criteria

1. `cargo test -p familyclaw-bridge --test homepage_factory` passes with `run_with(&live_executor)` behind optional `live-llm` feature or ignored integration test.
2. No changes to orchestrator, contract board, or gateway required for swap.
3. LLM + transport wired through existing agent stack (not duplicated in bridge).
4. Mock-LLM unit tests run in CI without API keys.

## References

- [`docs/HOMEPAGE_FACTORY.md`](../HOMEPAGE_FACTORY.md) — "Mock today, live tomorrow"
- [`docs/plans/2026-06-04-agent_gamma-amplifier-plan.md`](2026-06-04-agent_gamma-amplifier-plan.md) — memory amplifier follow-ups

## Cursor integration checklist (post-merge)

```powershell
cd E:\Familyclaw
cargo test --workspace
cargo run -p familyclaw-bench --bin bench -- all
cargo run -p familyclaw-bench --bin bench -- compare
powershell -File scripts/public-demo.ps1 -Full
.\scripts\homepage-factory-live-smoke.ps1 -CompareBench
```

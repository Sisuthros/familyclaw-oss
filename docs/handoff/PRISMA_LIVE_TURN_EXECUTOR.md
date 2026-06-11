# agent_gamma handoff — LiveTurnExecutor

**Owner:** agent_gamma (Layer B producer)  
**Integrator:** Cursor (after PR + review + CI green)  
**OSS release (Cursor):** Kerros A julkaisu — CI vihreä, geneeriset nimet, ei perheprofiileja repossa. **Ei koske tätä handoffia.**

## Cursor rule (until PR lands)

Do **not** edit `crates/familyclaw-agent/**` or `crates/familyclaw-memory/**` until agent_gamma's PR merges.

## agent_gamma scope (Layer B)

| Task | Crate / path | Notes |
|------|----------------|-------|
| `LiveTurnExecutor` | `familyclaw-agent` | Implements [`TurnExecutor`](../../crates/familyclaw-bridge/src/executor.rs) |
| Memory amplifier follow-ups | `familyclaw-memory` | See [amplifier plan](../plans/2026-06-04-agent_gamma-amplifier-plan.md) |
| Optional live smoke | `scripts/homepage-factory-live-smoke.ps1` | Behind `live-llm` feature or ignored test |

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
- [`docs/plans/2026-06-04-agent_gamma-amplifier-plan.md`](../plans/2026-06-04-agent_gamma-amplifier-plan.md) — memory amplifier follow-ups

## Cursor integration checklist (post-merge)

```powershell
cd E:\Familyclaw
cargo test --workspace
cargo run -p familyclaw-bench --bin bench -- all
cargo run -p familyclaw-bench --bin bench -- compare
powershell -File scripts/public-demo.ps1 -Full
.\scripts\homepage-factory-live-smoke.ps1 -CompareBench
```

## OSS vs perhe (2026-06-11)

- **GitHub / Kerros A:** `agent_a`, `agent_b`, `agent_alpha` … — julkaisuvalmis ilman `.env`:ää tai SOUL-tiedostoja.
- **Perhe / Kerros B:** `E:\familyclaw-profiles`, Telegram, agent_alpha SOUL — operator, ei blokkaa OSS-mergeä.

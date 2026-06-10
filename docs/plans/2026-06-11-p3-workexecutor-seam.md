# P3 — WorkExecutor seam (Homepage Factory) — ready-to-execute plan

> Status: **PLANNED, not implemented.** Authored 2026-06-11 by the night run
> after verifying P1/P2 are already done and P4.1/P5.1/P5.2 were committed by a
> concurrent run. P3 is the only open *code* item on the night priority list.
>
> **Blocker for execution:** this needs `cargo` to TDD + compile against the
> MSVC toolchain (`cargo +stable-x86_64-pc-windows-msvc test --workspace`). The
> autonomous run that wrote this plan did not have cargo authorized, so it
> stopped here rather than commit unverified Rust. Baseline: ~760 tests green —
> all must stay green.

## Goal

Introduce a `WorkExecutor` trait so task execution is abstracted behind a seam
(Layer-A producer side of the Homepage Factory). Ship a default *simulating*
executor so the workspace stays green, plus an integration test. The live
executor (Layer B) is **not** in scope here — that is agent_gamma's `live_executor.rs`
(lesson 899d2cee: producer/verifier split). We build only the Claude-side seam.

## Verified facts (file:line evidence gathered 2026-06-11)

- **No `WorkExecutor`, no `orchestrator.rs`, no `homepage_factory` exist yet.**
  The plan's old reference "orchestrator.rs:579-580" is stale.
- Task infra lives in `crates/familyclaw-bridge/src/task.rs`:
  - `Task { id, title, description, assignee: Option<AgentId>, status, created_at, updated_at }`
    (task.rs:70-94)
  - `TaskStatus { Pending, Active, Done, Handed }` with `can_transition_to`
    state machine (task.rs:38-66): `Pending→Active|Handed|Done`,
    `Active→Done|Handed`, `Handed→Active|Done`.
  - `TaskBoard { inner: Arc<RwLock<HashMap<TaskId, Task>>> }` (task.rs:138-150),
    async API: `update_status(id, next) -> Result<Task>` (task.rs:246-264),
    `assign(id, Option<AgentId>) -> Result<Task>` (task.rs:322-332).
- Async-trait style template: `Subject` trait in
  `crates/familyclaw-bench/src/subject.rs:147-182` (`#[async_trait] pub trait …`,
  `&mut self`, `-> Result<…>`), with a concrete impl in
  `crates/familyclaw-bench/src/subjects/familyclaw.rs:105-140`. Mirror this style.
- Sync trait precedent for a "deny-by-default, swappable impl": `CodeSandbox`
  in `crates/familyclaw-sandbox/src/sandbox.rs:143`.
- **Crate placement: `familyclaw-bridge`.** It already owns Task/TaskBoard, is
  already async (`tokio::sync::RwLock`), and is the composition layer.
  Its `Cargo.toml` deps (crates/familyclaw-bridge/Cargo.toml:14-24) currently
  lack `async-trait` — the workspace already declares `async-trait = "0.1"`
  (root Cargo.toml:28), so add `async-trait = { workspace = true }` to bridge.

## Implementation steps (TDD — red → green per step)

### P3.1 — `WorkExecutor` trait
- Add `async-trait = { workspace = true }` to
  `crates/familyclaw-bridge/Cargo.toml`.
- New module `crates/familyclaw-bridge/src/executor.rs`, re-exported from
  `lib.rs`.
- Define result + error types (Layer-A clean — no provider/family specifics):
  ```rust
  /// Outcome of executing one unit of work.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct WorkOutcome {
      pub task_id: TaskId,
      pub output: String,      // produced artifact / summary (generic)
      pub succeeded: bool,
  }

  #[async_trait::async_trait]
  pub trait WorkExecutor: Send + Sync {
      /// Executes a single Active task, producing an outcome.
      /// Implementors MUST NOT mutate TaskBoard themselves — the caller
      /// (driver) owns status transitions, keeping the seam side-effect-free.
      async fn execute(&self, task: &Task) -> Result<WorkOutcome>;
  }
  ```
- Doc the Layer-A/B boundary on the trait: Layer B supplies the live executor;
  Layer A only knows the trait + a simulating default.
- Tests (red first): trait object is usable (`Box<dyn WorkExecutor>`), outcome
  carries the task id.

### P3.2 — `DefaultSimulatingExecutor`
- In the same module: a deterministic, no-network executor that produces a
  predictable `WorkOutcome` (e.g. `output = format!("simulated: {}", task.title)`,
  `succeeded = true`). No clocks, no randomness (keep replay/test-determinism;
  note `Date.now()`/random are banned in some harness contexts — keep it pure).
- This keeps the existing ~760 tests green and gives integration tests a stable
  double.
- Tests: simulating executor returns `succeeded == true` and echoes the title;
  is `Send + Sync`; works behind `Box<dyn WorkExecutor>`.

### P3.3 — Integration test (homepage_factory seam)
- `crates/familyclaw-bridge/tests/homepage_factory.rs` (integration test, not a
  new prod file): drive a small flow —
  1. Create a `Task` on a `TaskBoard`, `update_status(Pending→Active)`.
  2. Run it through `DefaultSimulatingExecutor::execute`.
  3. On `outcome.succeeded`, caller does `update_status(Active→Done)`.
  4. Assert final board state is `Done` and the outcome echoes the task.
- This proves the seam composes with TaskBoard without baking execution inline.

## Gates before commit (MANDATORY)
1. `cargo +stable-x86_64-pc-windows-msvc build --workspace`
2. `cargo +stable-x86_64-pc-windows-msvc test --workspace` — all ~760+ green.
3. `cargo clippy --workspace --all-targets -- -D warnings` (baseline is 0).
4. Security gate on `git diff`: no souls, no keys (sk-/nvapi-/xai-/tp-/ghp_),
   no real Discord/Telegram IDs (17-19 digits), no private paths
   (E:\agent_alpha\workspace, C:\Users\operator, /root/.hermes, /mnt/d/agent_alpha).
5. Commit in Finnish (conventional), branch `feat/night-2026-06-11`, stage only
   the touched source files (no `git add .`). One item → one commit.

## Out of scope (do NOT touch)
- agent_gamma's `live_executor.rs` (Layer B live execution).
- `/profiles`, `hearth/`, `*.b64`, souls, keys, family/Hetzner infra.
- main-merge, force-push.

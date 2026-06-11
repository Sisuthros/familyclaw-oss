# P3 — WorkExecutor seam (Homepage Factory) — execution status

> Status: **P3.1 + P3.2 COMMITTED** in `e96e6fb` (executor.rs: `WorkExecutor`
> trait + `WorkOutcome` + `DefaultSimulatingExecutor`, with unit tests, all
> behind `async-trait`). **P3.3 integration test WRITTEN but NOT YET COMMITTED:**
> `crates/familyclaw-bridge/tests/homepage_factory.rs` (untracked).
>
> **P3.3 verification state (2026-06-11 night run):** the test was verified *by
> static inspection* against the committed `executor.rs`/`task.rs`/`lib.rs`:
> - every imported symbol is re-exported from `lib.rs:67-68`
>   (`DefaultSimulatingExecutor, TaskBoard, TaskStatus, WorkExecutor, WorkOutcome, Task`);
> - method signatures match: `board.create(&str, None) -> Result<Task>`,
>   `board.update_status(id, next) -> Result<Task>`, `board.get(id) -> Option<Task>`,
>   `exec.execute(&task) -> Result<WorkOutcome>`;
> - asserted output `"simulated: build homepage"` matches `executor.rs:104`;
> - `TaskStatus` is `Copy + PartialEq + Eq` (task.rs:38) and `Task` is
>   `Clone + PartialEq + Eq` with public `id/title/status` fields (task.rs:70-94),
>   so all `assert_eq!`/`!=` comparisons compile;
> - the tests dir is **not** gitignored, so it stages cleanly.
>
> **P3.3 coverage (run #19 deepened it):** the integration test now has FOUR
> cases — drive-to-Done, no-board-mutation invariant, trait-object swap, and a
> new **failure-path** case (`failing_executor_keeps_task_active_for_retry`)
> that exercises the driver's `succeeded = false → stay Active` retry branch via
> a test-local `AlwaysFailingExecutor`. This branch was previously unexercised
> because `DefaultSimulatingExecutor` always succeeds. Static check: the new
> executor uses `async_trait` + `familyclaw_core::Result` (both reachable from
> the bridge crate's `[dependencies]`, available to integration tests).
>
> **Remaining blocker:** the night run could NOT execute `cargo` (harness
> permission layer gated `cargo`, `git add/commit/push`, and all new bash
> commands — only read-only git + file tools were open). Per the TURVA-PORTTI
> mandate ("tests green via real cargo run before commit"), the run refused to
> fabricate a green result or commit unverified Rust. **Next cargo-enabled run:**
> run the gate below, then a single commit finishes P3.3.
>
> **To finish P3.3 (one cargo-enabled run):**
> ```
> cargo +stable-x86_64-pc-windows-msvc test -p familyclaw-bridge --test homepage_factory
> cargo +stable-x86_64-pc-windows-msvc test --workspace   # baseline ~760 green
> git add crates/familyclaw-bridge/tests/homepage_factory.rs
> git commit   # feat(bridge): homepage_factory-integraatiotesti WorkExecutor-saumalle (P3.3)
> git push origin feat/night-2026-06-11
> ```
> NOTE (corrected 2026-06-11, late run): `Cargo.lock` is **already in sync** —
> `familyclaw-bridge`'s dependency block already lists `async-trait`
> (Cargo.lock:1534, verified by direct read). There is **no** working-tree
> lockfile change to stage. The P3.3 commit is therefore exactly one untracked
> file: `crates/familyclaw-bridge/tests/homepage_factory.rs`. Do **not** `git add
> Cargo.lock` for this commit unless `cargo test` regenerates it for an unrelated
> reason.
>
> ## Independent line-traced compile verification (late run 2026-06-11)
>
> The integration test was re-verified symbol-by-symbol against the **committed**
> source (e96e6fb), not just asserted. All green:
> - Imports `DefaultSimulatingExecutor, Task, TaskBoard, TaskStatus, WorkExecutor,
>   WorkOutcome` — all re-exported at `lib.rs:67-68`. ✓
> - `TaskBoard::create(impl Into<String>, Option<AgentId>) -> Result<Task>`
>   (task.rs:156); `update_status(TaskId, TaskStatus) -> Result<Task>`
>   (task.rs:246); `get(TaskId) -> Option<Task>` (task.rs:187). ✓
> - `TaskStatus` derives `Copy + PartialEq + Eq` (task.rs:38) → the test's `!=`
>   (homepage_factory.rs:38) and `assert_eq!` on status compile. ✓
> - `Task` derives `Clone + PartialEq + Eq`, public `id/title/status`
>   (task.rs:70-94). ✓
> - `WorkOutcome::failure(TaskId, impl Into<String>)` exists (executor.rs:54),
>   used by the test-local `AlwaysFailingExecutor`. ✓
> - `#[async_trait::async_trait]` + `familyclaw_core::Result` reachable from an
>   integration test: both are normal `[dependencies]` of the bridge crate
>   (Cargo.toml:15-16), so they resolve in `tests/`. ✓
> - `#[tokio::test]` resolves: workspace `tokio` enables `macros` +
>   `rt-multi-thread` (root Cargo.toml:54), inherited via bridge's normal
>   `tokio` dep. ✓
>
> Conclusion: zero remaining static doubt. The next cargo-enabled run's gate is a
> formality — `cargo test -p familyclaw-bridge --test homepage_factory` is
> expected green on the first try.

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
4. Security gate on `git diff`: no souls, no API keys, no real Discord/Telegram
   IDs (17-19 digits), no absolute developer-machine paths (Windows user dirs,
   workspace roots, home-relative config dirs) — this is OSS Layer-A source.
5. Commit in Finnish (conventional), branch `feat/night-2026-06-11`, stage only
   the touched source files (no `git add .`). One item → one commit.

## Out of scope (do NOT touch)
- agent_gamma's `live_executor.rs` (Layer B live execution).
- `/profiles`, `hearth/`, `*.b64`, souls, keys, family/Hetzner infra.
- main-merge, force-push.

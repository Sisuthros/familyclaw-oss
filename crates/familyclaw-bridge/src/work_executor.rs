//! Work execution seam: the [`WorkExecutor`] trait and its default
//! implementation [`DefaultSimulatingExecutor`].
//!
//! This is the Homepage Factory's **Layer A** side (producer): an abstract
//! interface for executing a single task, decoupled from any concrete
//! implementation. Live execution (Layer B) plugs in behind the same trait as
//! a separate implementation (e.g. an actual LLM/tool call) — this crate does
//! not contain that; it contains only the seam and a deterministic simulating
//! default implementation.
//!
//! ## Seam contract
//! Implementations **do not** mutate the [`TaskBoard`](crate::TaskBoard)
//! themselves: the caller (the driver) owns the state transitions. This keeps
//! the seam free of side effects and testable, and lets the same executor be
//! run as a dry run without mutating the board.
//!
//! ## OSS boundary (Layer A)
//! Types are generic: no provider, model, souls, keys, or personal paths.
//! [`WorkOutcome::output`] is a free-form string (produced artifact / summary).

use crate::task::{Task, TaskId};
use familyclaw_core::Result;

/// The outcome of executing a single unit of work.
///
/// Contains the executed task's identifier, the produced (generic) output,
/// and a success flag. Kept as a plain data value (no clocks, no randomness)
/// so that replay and tests stay deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkOutcome {
    /// The executed task's stable identifier.
    pub task_id: TaskId,

    /// The produced artifact or summary (generic, free-form).
    pub output: String,

    /// Whether the execution succeeded.
    pub succeeded: bool,
}

impl WorkOutcome {
    /// Builds a successful outcome.
    #[must_use]
    pub fn success(task_id: TaskId, output: impl Into<String>) -> Self {
        Self {
            task_id,
            output: output.into(),
            succeeded: true,
        }
    }

    /// Builds a failed outcome.
    #[must_use]
    pub fn failure(task_id: TaskId, output: impl Into<String>) -> Self {
        Self {
            task_id,
            output: output.into(),
            succeeded: false,
        }
    }
}

/// The seam that executes a single task.
///
/// An implementation receives a task in the
/// [`Active`](crate::TaskStatus::Active) state and produces a [`WorkOutcome`].
/// **The implementation does not mutate the task board** — the caller owns
/// the state transitions (see the module documentation).
///
/// Layer B supplies the live executor; Layer A only knows this trait and the
/// deterministic [`DefaultSimulatingExecutor`] default.
#[async_trait::async_trait]
pub trait WorkExecutor: Send + Sync {
    /// Executes a single task and produces the outcome.
    ///
    /// # Errors
    /// Returns an error if execution fails in a way that does not fit the
    /// [`WorkOutcome`] `succeeded = false` semantics (e.g. an internal
    /// invariant is violated). An ordinary "the work did not succeed" is
    /// expressed with `Ok(WorkOutcome { succeeded: false, .. })`.
    async fn execute(&self, task: &Task) -> Result<WorkOutcome>;
}

/// A deterministic default implementation that simulates execution without
/// any network access.
///
/// Produces a predictable [`WorkOutcome`] (`output = "simulated: {title}"`,
/// `succeeded = true`) without reading the clock or using randomness. Keeps
/// existing tests green and gives integration tests a stable double without
/// the live layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultSimulatingExecutor;

impl DefaultSimulatingExecutor {
    /// Creates a new simulating executor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl WorkExecutor for DefaultSimulatingExecutor {
    async fn execute(&self, task: &Task) -> Result<WorkOutcome> {
        Ok(WorkOutcome::success(
            task.id,
            format!("simulated: {}", task.title),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskBoard;

    /// Helper: creates a Pending task on a fresh board and returns it.
    async fn pending_task(title: &str) -> Task {
        let board = TaskBoard::new();
        board.create(title, None).await.expect("create task")
    }

    #[tokio::test]
    async fn outcome_carries_task_id() {
        let task = pending_task("write homepage").await;
        let outcome = WorkOutcome::success(task.id, "done");
        assert_eq!(outcome.task_id, task.id);
        assert!(outcome.succeeded);
        assert_eq!(outcome.output, "done");
    }

    #[tokio::test]
    async fn failure_outcome_is_not_succeeded() {
        let task = pending_task("flaky job").await;
        let outcome = WorkOutcome::failure(task.id, "boom");
        assert!(!outcome.succeeded);
        assert_eq!(outcome.task_id, task.id);
    }

    #[tokio::test]
    async fn simulating_executor_echoes_title_and_succeeds() {
        let task = pending_task("ship the seed").await;
        let exec = DefaultSimulatingExecutor::new();
        let outcome = exec.execute(&task).await.expect("execute");
        assert!(outcome.succeeded);
        assert_eq!(outcome.output, "simulated: ship the seed");
        assert_eq!(outcome.task_id, task.id);
    }

    #[tokio::test]
    async fn simulating_executor_is_deterministic() {
        let task = pending_task("same input").await;
        let exec = DefaultSimulatingExecutor::new();
        let a = exec.execute(&task).await.expect("execute a");
        let b = exec.execute(&task).await.expect("execute b");
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn usable_behind_trait_object() {
        // Core of the seam: the executor is usable as `Box<dyn WorkExecutor>`,
        // so that Layer B's live implementation can later be dropped in its place.
        let task = pending_task("dyn dispatch").await;
        let exec: Box<dyn WorkExecutor> = Box::new(DefaultSimulatingExecutor::new());
        let outcome = exec.execute(&task).await.expect("execute via dyn");
        assert!(outcome.succeeded);
        assert_eq!(outcome.task_id, task.id);
    }

    #[test]
    fn executor_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DefaultSimulatingExecutor>();
    }
}

//! Integration test: the [`WorkExecutor`] seam composes with [`TaskBoard`].
//!
//! Proves the Homepage Factory's Layer A seam: task execution is decoupled
//! from the board. The driver runs the task through the executor and owns
//! the state transitions; the executor does not mutate the board. When the
//! live executor (Layer B) is later dropped in place of
//! `DefaultSimulatingExecutor`, this flow stays the same.

use familyclaw_bridge::{
    DefaultSimulatingExecutor, Task, TaskBoard, TaskStatus, WorkExecutor, WorkOutcome,
};

/// A test-internal executor that always fails — proves the driver's
/// retry branch (`succeeded = false` leaves the task in the Active state).
/// Layer A does not ship anything like this; it exists only to demonstrate the test's invariant.
struct AlwaysFailingExecutor;

#[async_trait::async_trait]
impl WorkExecutor for AlwaysFailingExecutor {
    async fn execute(&self, task: &Task) -> familyclaw_core::Result<WorkOutcome> {
        Ok(WorkOutcome::failure(task.id, "simulated failure"))
    }
}

/// A small driver: runs an Active task through the executor and transitions
/// its status based on the outcome. This is the seam's caller side — *the executor does not do this*.
async fn run_one(board: &TaskBoard, exec: &dyn WorkExecutor, task: &Task) -> WorkOutcome {
    let outcome = exec.execute(task).await.expect("execute task");
    let next = if outcome.succeeded {
        TaskStatus::Done
    } else {
        // A failure leaves the task in the Active state for a retry.
        TaskStatus::Active
    };
    if next != task.status {
        board
            .update_status(task.id, next)
            .await
            .expect("driver transitions status");
    }
    outcome
}

#[tokio::test]
async fn simulating_executor_drives_task_to_done() {
    let board = TaskBoard::new();
    let exec = DefaultSimulatingExecutor::new();

    // 1. Create the task and pick it up.
    let task = board.create("build homepage", None).await.expect("create");
    assert_eq!(task.status, TaskStatus::Pending);
    let active = board
        .update_status(task.id, TaskStatus::Active)
        .await
        .expect("activate");
    assert_eq!(active.status, TaskStatus::Active);

    // 2-3. Execute through the seam; on success, the driver transitions to Done.
    let outcome = run_one(&board, &exec, &active).await;

    // 4. The outcome echoes the task, and the board ends up in the Done state.
    assert!(outcome.succeeded);
    assert_eq!(outcome.task_id, active.id);
    assert_eq!(outcome.output, "simulated: build homepage");

    let finished = board.get(active.id).await.expect("task exists");
    assert_eq!(finished.status, TaskStatus::Done);
}

#[tokio::test]
async fn executor_does_not_mutate_the_board_itself() {
    // Sauman invariantti: suorittaja ei kosketa taulua. Ajetaan execute ilman
    // driver — the board's status must remain Active.
    let board = TaskBoard::new();
    let exec = DefaultSimulatingExecutor::new();

    let task = board.create("no side effects", None).await.expect("create");
    let active = board
        .update_status(task.id, TaskStatus::Active)
        .await
        .expect("activate");

    let _ = exec.execute(&active).await.expect("execute");

    let still = board.get(active.id).await.expect("task exists");
    assert_eq!(
        still.status,
        TaskStatus::Active,
        "executor must not transition the task itself"
    );
}

#[tokio::test]
async fn failing_executor_keeps_task_active_for_retry() {
    // Driverin uudelleenyritys-haara: kun suorittaja palauttaa
    // `succeeded = false`, the driver does NOT transition the task to Done
    // but leaves it in the Active state. Proves that the outcome — not a
    // side effect from the executor — drives the state transition.
    let board = TaskBoard::new();
    let exec = AlwaysFailingExecutor;

    let task = board.create("flaky build", None).await.expect("create");
    let active = board
        .update_status(task.id, TaskStatus::Active)
        .await
        .expect("activate");

    let outcome = run_one(&board, &exec, &active).await;
    assert!(!outcome.succeeded);
    assert_eq!(outcome.task_id, active.id);

    let after = board.get(active.id).await.expect("task exists");
    assert_eq!(
        after.status,
        TaskStatus::Active,
        "failed outcome must leave the task Active for retry, not advance it"
    );
}

#[tokio::test]
async fn seam_accepts_swapped_executor_via_trait_object() {
    // Layer B's drop-in replaceability: the driver works with any
    // `dyn WorkExecutor`. Here we use the default double, but the type is `Box<dyn ...>`.
    let board = TaskBoard::new();
    let exec: Box<dyn WorkExecutor> = Box::new(DefaultSimulatingExecutor::new());

    let task = board.create("swap me", None).await.expect("create");
    let active = board
        .update_status(task.id, TaskStatus::Active)
        .await
        .expect("activate");

    let outcome = run_one(&board, exec.as_ref(), &active).await;
    assert!(outcome.succeeded);

    let finished = board.get(active.id).await.expect("task exists");
    assert_eq!(finished.status, TaskStatus::Done);
}

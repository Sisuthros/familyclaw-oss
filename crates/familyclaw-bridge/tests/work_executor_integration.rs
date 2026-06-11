//! Integraatiotesti: [`WorkExecutor`]-sauma komponoituu [`TaskBoard`]in kanssa.
//!
//! Todistaa Homepage Factoryn KERROS A -sauman: tehtävän suoritus on irrotettu
//! taulusta. Driver ajaa tehtävän suorittajan läpi ja omistaa tilasiirtymät;
//! suorittaja ei mutatoi taulua. Kun live-suorittaja (KERROS B) myöhemmin
//! pudotetaan `DefaultSimulatingExecutor`:n tilalle, tämä virtaus pysyy samana.

use familyclaw_bridge::{
    DefaultSimulatingExecutor, Task, TaskBoard, TaskStatus, WorkExecutor, WorkOutcome,
};

/// Testin sisäinen suorittaja, joka aina epäonnistuu — todistaa driverin
/// uudelleenyritys-haaran (`succeeded = false` jättää tehtävän Active-tilaan).
/// KERROS A ei toimita tällaista; tämä elää vain testin invariantin osoittamiseksi.
struct AlwaysFailingExecutor;

#[async_trait::async_trait]
impl WorkExecutor for AlwaysFailingExecutor {
    async fn execute(&self, task: &Task) -> familyclaw_core::Result<WorkOutcome> {
        Ok(WorkOutcome::failure(task.id, "simulated failure"))
    }
}

/// Pieni driver: ajaa Active-tehtävän suorittajan läpi ja siirtää tilan
/// lopputuloksen mukaan. Sauman kutsujapuoli — *suorittaja ei tee tätä*.
async fn run_one(board: &TaskBoard, exec: &dyn WorkExecutor, task: &Task) -> WorkOutcome {
    let outcome = exec.execute(task).await.expect("execute task");
    let next = if outcome.succeeded {
        TaskStatus::Done
    } else {
        // Epäonnistuminen jättää tehtävän Active-tilaan uudelleenyritystä varten.
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

    // 1. Luo tehtävä ja ota työn alle.
    let task = board.create("build homepage", None).await.expect("create");
    assert_eq!(task.status, TaskStatus::Pending);
    let active = board
        .update_status(task.id, TaskStatus::Active)
        .await
        .expect("activate");
    assert_eq!(active.status, TaskStatus::Active);

    // 2-3. Suorita sauman läpi; driver siirtää onnistuessa Done:ksi.
    let outcome = run_one(&board, &exec, &active).await;

    // 4. Lopputulos kaikuttaa tehtävän, ja taulu päätyy Done-tilaan.
    assert!(outcome.succeeded);
    assert_eq!(outcome.task_id, active.id);
    assert_eq!(outcome.output, "simulated: build homepage");

    let finished = board.get(active.id).await.expect("task exists");
    assert_eq!(finished.status, TaskStatus::Done);
}

#[tokio::test]
async fn executor_does_not_mutate_the_board_itself() {
    // Sauman invariantti: suorittaja ei kosketa taulua. Ajetaan execute ilman
    // driveriä — taulun tilan on pysyttävä Active:na.
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
    // `succeeded = false`, driver EI siirrä tehtävää Done:ksi vaan jättää sen
    // Active-tilaan. Todistaa että lopputulos — ei suorittajan sivuvaikutus —
    // ohjaa tilasiirtymän.
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
    // Kerros B:n pudotettavuus: driver toimii minkä tahansa `dyn WorkExecutor`:n
    // kanssa. Tässä käytämme oletustuplaa, mutta tyyppi on `Box<dyn …>`.
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

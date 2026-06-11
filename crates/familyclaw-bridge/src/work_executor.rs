//! Työn suoritussauma: [`WorkExecutor`]-trait ja oletustoteutus
//! [`DefaultSimulatingExecutor`].
//!
//! Tämä on Homepage Factoryn **KERROS A** -puoli (producer): abstrakti rajapinta
//! yhden tehtävän suoritukselle, irrotettuna konkreettisesta toteutuksesta.
//! Live-suoritus (KERROS B) tulee saman traitin taakse erillisenä toteutuksena
//! (esim. todellinen LLM-/työkalukutsu) — tätä crate ei sisällä, vain saumaa ja
//! deterministisen simuloivan oletustoteutuksen.
//!
//! ## Sauman sopimus
//! Toteuttajat **eivät** mutatoi [`TaskBoard`](crate::TaskBoard)ia itse: kutsuja
//! (driver) omistaa tilasiirtymät. Näin sauma pysyy sivuvaikutuksettomana ja
//! testattavana, ja sama suorittaja voidaan ajaa kuivaharjoituksena ilman
//! taulun mutatointia.
//!
//! ## OSS-raja (KERROS A)
//! Tyypit ovat geneerisiä: ei provideria, mallia, sieluja, avaimia eikä
//! henkilökohtaisia polkuja. [`WorkOutcome::output`] on vapaamuotoinen merkkijono
//! (tuotettu artefakti / tiivistelmä).

use crate::task::{Task, TaskId};
use familyclaw_core::Result;

/// Yhden työyksikön suorituksen lopputulos.
///
/// Sisältää suoritetun tehtävän tunnisteen, tuotetun (geneerisen) tulosteen ja
/// onnistumislipun. Pidetään puhtaana data-arvona (ei kelloja, ei satunnaisuutta)
/// jotta replay ja testit pysyvät deterministisinä.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkOutcome {
    /// Suoritetun tehtävän vakaa tunniste.
    pub task_id: TaskId,

    /// Tuotettu artefakti tai tiivistelmä (geneerinen, vapaamuotoinen).
    pub output: String,

    /// Onnistuiko suoritus.
    pub succeeded: bool,
}

impl WorkOutcome {
    /// Rakentaa onnistuneen lopputuloksen.
    #[must_use]
    pub fn success(task_id: TaskId, output: impl Into<String>) -> Self {
        Self {
            task_id,
            output: output.into(),
            succeeded: true,
        }
    }

    /// Rakentaa epäonnistuneen lopputuloksen.
    #[must_use]
    pub fn failure(task_id: TaskId, output: impl Into<String>) -> Self {
        Self {
            task_id,
            output: output.into(),
            succeeded: false,
        }
    }
}

/// Yhden tehtävän suorittava sauma.
///
/// Toteuttaja saa [`Active`](crate::TaskStatus::Active)-tilaisen tehtävän ja
/// tuottaa [`WorkOutcome`]:n. **Toteuttaja ei mutatoi tehtävätaulua** — kutsuja
/// omistaa tilasiirtymät (ks. moduulin dokumentaatio).
///
/// KERROS B toimittaa live-suorittajan; KERROS A tuntee vain tämän traitin ja
/// deterministisen [`DefaultSimulatingExecutor`]-oletuksen.
#[async_trait::async_trait]
pub trait WorkExecutor: Send + Sync {
    /// Suorittaa yhden tehtävän ja tuottaa lopputuloksen.
    ///
    /// # Errors
    /// Palauttaa virheen jos suoritus epäonnistuu tavalla, joka ei mahdu
    /// [`WorkOutcome`]:n `succeeded = false` -semantiikkaan (esim. sisäinen
    /// invariantti rikkoutuu). Tavallinen "työ ei onnistunut" ilmaistaan
    /// `Ok(WorkOutcome { succeeded: false, .. })`:llä.
    async fn execute(&self, task: &Task) -> Result<WorkOutcome>;
}

/// Deterministinen, verkotonta suoritusta simuloiva oletustoteutus.
///
/// Tuottaa ennustettavan [`WorkOutcome`]:n (`output = "simulated: {otsikko}"`,
/// `succeeded = true`) lukematta kelloa tai satunnaisuutta. Pitää olemassa olevat
/// testit vihreinä ja tarjoaa integraatiotesteille vakaan tuplan ilman live-kerrosta.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultSimulatingExecutor;

impl DefaultSimulatingExecutor {
    /// Luo uuden simuloivan suorittajan.
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

    /// Apuri: luo Pending-tehtävän tuoreelle taululle ja palauttaa sen.
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
        // Sauman ydin: suorittaja on käytettävissä `Box<dyn WorkExecutor>`:na,
        // jotta KERROS B:n live-toteutus voidaan myöhemmin pudottaa tilalle.
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

//! [`Harness`]: ajaa skenaariot subjektia vasten ja kokoaa [`Scorecard`]:n.
//!
//! Harness on saumaton (design §2.1): se ei tiedä *kuka* subjekti on eikä
//! *mitä* skenaario tekee — se vain ajaa `Scenario × Subject → ScenarioResult`
//! ja aggregoi tulokset. Kello on injektoitu, joten ajo on reprodusoitava.

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::scenario::Scenario;
use crate::scorecard::Scorecard;
use crate::subject::Subject;

/// Skenaarioajojen suorittaja ja scorecardin koostaja.
///
/// Tilaton — koko ajotila kulkee parametreina, jotta sama kutsu tuottaa saman
/// tuloksen (design §2.2).
#[derive(Debug, Default, Clone, Copy)]
pub struct Harness;

impl Harness {
    /// Rakentaa uuden harnessin.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Ajaa kaikki skenaariot annettua subjektia vasten ja palauttaa
    /// aggregoidun [`Scorecard`]:n.
    ///
    /// `clock` on injektoitu referenssihetki joka välitetään jokaiselle
    /// skenaariolle ja talletetaan scorecardiin — järjestelmäkelloa ei lueta.
    ///
    /// # Errors
    /// Palauttaa ensimmäisen skenaarion virheen joka epäonnistuu (`?`).
    pub async fn run(
        &self,
        subject: &mut dyn Subject,
        scenarios: &[Box<dyn Scenario>],
        clock: Timestamp,
    ) -> Result<Scorecard> {
        let name = subject.name().to_string();
        let mut results = Vec::with_capacity(scenarios.len());
        for scenario in scenarios {
            let result = scenario.run(subject, clock).await?;
            results.push(result);
        }
        Ok(Scorecard::new(name, results, clock))
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub-toteutukset palauttavat literaaleja.
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;

    use super::*;
    use crate::scenario::ScenarioResult;
    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(Vec::new())
        }
        async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
            Ok(DreamSummary {
                scanned: 0,
                merged: 0,
                dropped: 0,
                dates_absolutized: 0,
                strengthened: 0,
                archived: 0,
                protected_core_intact: true,
            })
        }
        fn name(&self) -> &str {
            "stub_subject"
        }
    }

    struct StubScenario;

    #[async_trait]
    impl Scenario for StubScenario {
        fn id(&self) -> &str {
            "stub_scenario"
        }
        async fn run(
            &self,
            _subject: &mut dyn Subject,
            _clock: Timestamp,
        ) -> Result<ScenarioResult> {
            Ok(ScenarioResult {
                id: "stub_scenario".into(),
                passed: true,
                metrics: BTreeMap::new(),
                notes: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn harness_aggregates_results() {
        let clock = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        let mut subject = StubSubject;
        let scenarios: Vec<Box<dyn Scenario>> = vec![Box::new(StubScenario)];
        let card = Harness::new()
            .run(&mut subject, &scenarios, clock)
            .await
            .expect("run");
        assert_eq!(card.subject, "stub_subject");
        assert_eq!(card.scenarios.len(), 1);
        assert!(card.all_passed());
    }
}

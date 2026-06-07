//! S5 Semantic Retrieval — osittaisosuman semanttinen haku.
//!
//! Tämä skenaario todistaa että Eternal Thread löytää oikeat muistit
//! **merkityksen perusteella**, ei pelkästään tarkoilla avainsanoilla.
//!
//! ## Mitä testataan
//! Kylvetään kolme muistoa:
//! - "shipped the continuity bridge" (sisältää "ship" osamerkkijonona)
//! - "deploying the system update" (eri sanat)
//! - "ocean waves and songs" (täysin eri aihe)
//!
//! Kysely: "did we ship it?"
//! Odotettu: semanttinen haku (substring-match) nostaa "shipped"-muistin
//! relevantimmaksi kuin pelkkä keyword-haku, koska "ship" löytyy
//! osamerkkijonona sanasta "shipped".

use async_trait::async_trait;
use familyclaw_core::Timestamp;
use familyclaw_memory::{
    ImportanceFactors, LocalJsonStore, Memory, MemoryStore, RetrievalContext,
};

use crate::error::Result;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

#[derive(Debug, Default, Clone, Copy)]
pub struct SemanticRetrieval;

impl SemanticRetrieval {
    pub const ID: &'static str = "s5_semantic_retrieval";

    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

const MEM_SHIPPED: &str = "shipped the continuity bridge";
const MEM_DEPLOYING: &str = "deploying the system update";
const MEM_OCEAN: &str = "ocean waves and songs";
const QUERY: &str = "did we ship it";

#[async_trait]
impl Scenario for SemanticRetrieval {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, _subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let store = LocalJsonStore::in_memory();

        let mem_shipped = Memory::builder(MEM_SHIPPED)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .created_at(clock)
            .build();
        let id_shipped = store.add(mem_shipped).await?;

        let mem_deploy = Memory::builder(MEM_DEPLOYING)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .created_at(clock)
            .build();
        let _id_deploy = store.add(mem_deploy).await?;

        let mem_ocean = Memory::builder(MEM_OCEAN)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .created_at(clock)
            .build();
        let _id_ocean = store.add(mem_ocean).await?;

        // Hae ilman semantiikkaa
        let ctx_kw = RetrievalContext::new(QUERY)
            .with_limit(3)
            .with_semantic_weight(0.0);
        let hits_kw = store.retrieve(&ctx_kw, clock).await?;

        // Hae semantiikalla (substring-match)
        let ctx_sem = RetrievalContext::new(QUERY)
            .with_limit(3)
            .with_semantic_weight(0.7);
        let hits_sem = store.retrieve(&ctx_sem, clock).await?;

        let kw_relevance = hits_kw
            .iter()
            .find(|h| h.memory.id == id_shipped)
            .map(|h| h.relevance)
            .unwrap_or(0.0);
        let sem_relevance = hits_sem
            .iter()
            .find(|h| h.memory.id == id_shipped)
            .map(|h| h.relevance)
            .unwrap_or(0.0);
        let boost = (sem_relevance - kw_relevance).max(0.0);

        // Ocean-muisti EI saa nousta kärkeen kummallakaan haulla
        let kw_top_is_ocean = hits_kw
            .first()
            .is_some_and(|h| h.memory.content.contains("ocean"));
        let sem_top_is_ocean = hits_sem
            .first()
            .is_some_and(|h| h.memory.content.contains("ocean"));

        let top1_correct = hits_sem
            .first()
            .is_some_and(|h| h.memory.id == id_shipped);

        let passed = boost > 0.0 && top1_correct && !sem_top_is_ocean;

        let result = ScenarioResult::new(Self::ID, passed)
            .with_metric("semantic_boost", f64::from(boost))
            .with_metric("semantic_top1_is_shipped", if top1_correct { 1.0 } else { 0.0 })
            .with_note(format!(
                "keyword relevance(shipped)={kw_relevance:.3}, semantic relevance(shipped)={sem_relevance:.3}, boost={boost:.3}"
            ))
            .with_note(format!(
                "kw top-1 ocean={kw_top_is_ocean}, sem top-1 ocean={sem_top_is_ocean}"
            ));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::{
        CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task,
    };

    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _c: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _h: &RunHandle, _p: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _c: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _q: &str, _c: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(vec![RecallHit::new("memory", 0.5)])
        }
        async fn sleep_cycle(&mut self, _c: Timestamp) -> Result<DreamSummary> {
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
            "stub_s5"
        }
    }

    #[tokio::test]
    async fn semantic_retrieval_passes() {
        let scenario = SemanticRetrieval::new();
        let mut subject = StubSubject;
        let clock = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        let result = scenario.run(&mut subject, clock).await.expect("run");

        assert_eq!(result.id, SemanticRetrieval::ID);
        assert!(
            result.passed,
            "S5 should pass: {:?}",
            result.notes
        );
        let boost = result.metrics.get("semantic_boost").copied().unwrap_or(0.0);
        assert!(boost > 0.0, "semantic_boost must be > 0, got {boost}");
    }
}

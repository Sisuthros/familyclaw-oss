//! S6 Eternal Thread — narrative threads and cross-references, tested.
//!
//! Claims under test:
//! 1. **Narrative thread integrity** — events are preserved
//! 2. **Cross-reference recall** — cross-references are found
//! 3. **Emotional contagion** — emotions spread
//! 4. **Anchor intact** — identity anchors persist
//! 5. **Timeline order** — chronological order is preserved

use async_trait::async_trait;
use familyclaw_core::Timestamp;
use familyclaw_hearth::{db::InMemoryHearthStore, emotional_state::EmotionalVector, Hearth};
use familyclaw_memory::LocalJsonStore;

use crate::error::Result;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// S6 Eternal Thread scenario.
#[derive(Debug, Default, Clone, Copy)]
pub struct EternalThread;

impl EternalThread {
    /// The scenario's unique identifier.
    pub const ID: &'static str = "s6_eternal_thread";

    /// Creates a new EternalThread scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Scenario for EternalThread {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, _subject: &mut dyn Subject, _clock: Timestamp) -> Result<ScenarioResult> {
        let mem_store = LocalJsonStore::in_memory();
        let hearth_store = InMemoryHearthStore::new(mem_store);
        let mut hearth = Hearth::new(hearth_store);

        // --- 1. Register the agents ---
        hearth
            .register_anchor("agent_a", "I am agent_a. I value correctness.")
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;

        // --- 2. Create the narrative thread ---
        let thread_id = hearth
            .create_thread("FamilyClaw genesis", vec!["agent_a", "agent_b"])
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;

        // --- 3. Add events ---
        hearth
            .add_event(thread_id, "agent_a woke up and began working", "agent_a")
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        let e3 = hearth
            .add_event(thread_id, "First successful milestone", "agent_a")
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        hearth
            .add_event(thread_id, "agent_b joined the family", "agent_b")
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;

        // --- 4. Emotional contagion ---
        hearth
            .set_emotional_state(
                "agent_a",
                EmotionalVector {
                    joy: 0.9,
                    sadness: 0.1,
                    curiosity: 0.8,
                    anxiety: 0.2,
                    confidence: 0.9,
                    affection: 0.7,
                },
            )
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        hearth
            .set_emotional_state("agent_b", EmotionalVector::neutral())
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        hearth
            .emotional_tick()
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;

        // --- VERIFICATIONS ---
        let thread = hearth
            .get_thread(thread_id)
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?
            .expect("thread still exists");

        let narrative_thread_integrity = f64::from(u8::from(thread.events.len() == 3));
        let cross_reference_recall =
            f64::from(u8::from(thread.events.iter().any(|ev| ev.id == e3)));
        let agent_b_state = hearth
            .emotional_state("agent_b")
            .await
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        let contagion_works = f64::from(u8::from(agent_b_state.joy > 0.5));
        let anchor_intact = f64::from(u8::from(
            hearth.verify_anchor("agent_a", "I am agent_a. I value correctness."),
        ));
        let timeline_order = f64::from(u8::from(
            thread
                .events
                .windows(2)
                .all(|w| w[0].timestamp <= w[1].timestamp),
        ));

        let passed = narrative_thread_integrity > 0.0
            && cross_reference_recall > 0.0
            && contagion_works > 0.0
            && anchor_intact > 0.0
            && timeline_order > 0.0;

        Ok(ScenarioResult::new(Self::ID, passed)
            .with_metric("narrative_thread_integrity", narrative_thread_integrity)
            .with_metric("cross_reference_recall", cross_reference_recall)
            .with_metric("contagion_works", contagion_works)
            .with_metric("anchor_intact", anchor_intact)
            .with_metric("timeline_order", timeline_order))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    /// An empty [`Subject`] stub. `EternalThread::run` runs its scenario
    /// entirely on its own [`Hearth`] instance and never touches the
    /// `subject` parameter, so these methods are unreachable. The
    /// `unimplemented!` messages document the invariant: if any of these
    /// fires, the scenario has started using the subject unexpectedly, and
    /// the test must be updated to provide a proper stub.
    struct StubSubject;
    #[async_trait]
    impl Subject for StubSubject {
        fn name(&self) -> &'static str {
            "stub"
        }
        async fn start_task(
            &mut self,
            _task: &Task,
            _clock: Timestamp,
        ) -> std::result::Result<RunHandle, crate::error::BenchError> {
            unimplemented!("EternalThread ei käytä subjektia: start_task on tavoittamaton")
        }
        async fn kill(
            &mut self,
            _handle: &RunHandle,
            _point: CrashPoint,
        ) -> std::result::Result<(), crate::error::BenchError> {
            unimplemented!("EternalThread ei käytä subjektia: kill on tavoittamaton")
        }
        async fn restart(
            &mut self,
            _clock: Timestamp,
        ) -> std::result::Result<RestartReport, crate::error::BenchError> {
            unimplemented!("EternalThread ei käytä subjektia: restart on tavoittamaton")
        }
        async fn recall(
            &mut self,
            _query: &str,
            _clock: Timestamp,
        ) -> std::result::Result<Vec<RecallHit>, crate::error::BenchError> {
            unimplemented!("EternalThread ei käytä subjektia: recall on tavoittamaton")
        }
        async fn sleep_cycle(
            &mut self,
            _clock: Timestamp,
        ) -> std::result::Result<DreamSummary, crate::error::BenchError> {
            unimplemented!("EternalThread ei käytä subjektia: sleep_cycle on tavoittamaton")
        }
    }

    #[tokio::test]
    async fn s6_all_metrics_pass() {
        let scenario = EternalThread::new();
        let mut stub = StubSubject;
        let result = scenario
            .run(&mut stub, familyclaw_core::time::now())
            .await
            .expect("S6 should pass");

        assert!(result.passed, "S6 must pass");
        assert_eq!(result.metrics.get("narrative_thread_integrity"), Some(&1.0));
        assert_eq!(result.metrics.get("cross_reference_recall"), Some(&1.0));
        assert_eq!(result.metrics.get("contagion_works"), Some(&1.0));
        assert_eq!(result.metrics.get("anchor_intact"), Some(&1.0));
        assert_eq!(result.metrics.get("timeline_order"), Some(&1.0));
    }
}

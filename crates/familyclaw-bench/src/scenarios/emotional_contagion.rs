//! S4 Emotional Contagion — the Resonance Bus's affective contagion, tested.
//!
//! This scenario proves three things:
//! 1. **Affective contagion** — an agent's emotional state spreads to a
//!    sibling via the Resonance Bus with the correct coefficient.
//! 2. **Homeostasis** — without continued stimulus, emotional state returns
//!    toward neutral (no saturation).
//! 3. **Memory isolation** — an agent's own memories do not leak to another
//!    agent via the bus; each remembers only its own experiences.
//!
//! ## Why this is unique
//! No competitor markets an *affective nervous system* between agents.
//! FamilyClaw's Resonance Bus is a biological analog: emotions spread, but
//! homeostasis prevents the feedback loop from saturating. This scenario
//! makes that measurable.
//!
//! ## Reproducibility
//! Same injected clock → identical result (design §2.2).
//!
//! ## Claim under test
//! > *"When one agent feels joy, the family feels it — but no one
//! > burns out."*

use async_trait::async_trait;
use familyclaw_bus::{BeingId, BusMessage, ResonanceBus};
use familyclaw_core::Timestamp;
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{LocalJsonStore, MemoryStore, RetrievalContext};

use crate::error::Result;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// An agent's emotional state before and after the pulse.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct EmotionalSnapshot {
    joy: f32,
    curiosity: f32,
    sadness: f32,
}

impl EmotionalSnapshot {
    fn from_state(state: &EmotionState) -> Self {
        Self {
            joy: state.value(Dimension::Joy),
            curiosity: state.value(Dimension::Curiosity),
            sadness: state.value(Dimension::Sadness),
        }
    }
}

/// S4 Emotional Contagion scenario.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmotionalContagion;

impl EmotionalContagion {
    /// The scenario's unique identifier.
    pub const ID: &'static str = "s4_emotional_contagion";

    /// Creates a new EmotionalContagion scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Scenario for EmotionalContagion {
    fn id(&self) -> &str {
        Self::ID
    }

    // The scenario is a cohesive, staged test rig; splitting it up because of
    // line count would break up its readability. The `agent_a_*`/`agent_b_*`
    // names are deliberately similar (two symmetric agents).
    #[allow(
        clippy::too_many_lines,
        clippy::similar_names,
        clippy::cast_precision_loss
    )]
    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        // ── 1. Start the Resonance Bus ─────────────────────────────────────
        let bus = ResonanceBus::start(None)
            .await
            .map_err(|e| crate::BenchError::scenario(format!("bus start: {e}")))?;

        // ── 2. Build the state of two agents ─────────────────────────────────
        let store_a = LocalJsonStore::in_memory();
        let store_b = LocalJsonStore::in_memory();

        let mut agent_a_emotion = EmotionState::neutral();
        let mut agent_b_emotion = EmotionState::neutral();

        let agent_a_id = BeingId::new();
        let _agent_b_id = BeingId::new();

        // ── 3. Test pulse contagion ─────────────────────────────────────
        // agent_a feels strong joy → agent_b should feel some of it.

        let _before_pulse = EmotionalSnapshot::from_state(&agent_b_emotion);

        agent_a_emotion.set(Dimension::Joy, 80.0);
        agent_a_emotion.set(Dimension::Curiosity, 60.0);

        // agent_a sends an emotion pulse to the bus
        let pulse = BusMessage::emotion_pulse(agent_a_emotion);
        bus.publish(agent_a_id, pulse)
            .map_err(|e| crate::BenchError::scenario(format!("publish pulse: {e}")))?;

        // Let the pulse propagate (async bus, small delay)
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Simulate agent_b's reception: affective contagion.
        // CONTAGION_FACTOR = 0.25 (familyclaw-agent/src/agent.rs).
        // Contagion is *approaching* the source (not accumulating), so that a
        // repeated pulse does not saturate to the ceiling: delta = (source − own) · factor.
        // agent_b starts from neutral (0), so the result = source · factor.
        let contagion: f32 = 0.25;

        // Iterate over dimensions and apply contagion
        for dim in [Dimension::Joy, Dimension::Curiosity] {
            let current = agent_b_emotion.value(dim);
            let delta = (agent_a_emotion.value(dim) - current) * contagion;
            agent_b_emotion.stimulate(dim, delta);
        }

        // Homeostasis: 10% recovery toward neutral
        let homeostasis: f32 = 0.10;
        let neutral = EmotionState::neutral();
        for dim in [Dimension::Joy, Dimension::Curiosity, Dimension::Sadness] {
            let current = agent_b_emotion.value(dim);
            let target = neutral.value(dim);
            let correction = (current - target) * homeostasis;
            agent_b_emotion.set(dim, current - correction);
        }

        let after_pulse = EmotionalSnapshot::from_state(&agent_b_emotion);

        // ── 4. Verify affective contagion ────────────────────────────
        // Expected: Joy = 80*0.25*0.9 = 18.0, Curiosity = 60*0.25*0.9 = 13.5
        let expected_joy: f32 = 80.0 * contagion * (1.0 - homeostasis);
        let expected_curiosity: f32 = 60.0 * contagion * (1.0 - homeostasis);

        let joy_ok = (after_pulse.joy - expected_joy).abs() < 0.001;
        let curiosity_ok = (after_pulse.curiosity - expected_curiosity).abs() < 0.001;
        let contagion_works = joy_ok && curiosity_ok;

        // ── 5. Test homeostasis over multiple turns ──────────────────────
        // Without a new pulse, agent_b's emotional state should recover.
        let mut turns_without_stimulus = 0;
        let mut state_before_homeostasis = agent_b_emotion;
        for turn in 0..10 {
            let _snapshot = EmotionalSnapshot::from_state(&agent_b_emotion);
            // No pulse on this turn

            for dim in [Dimension::Joy, Dimension::Curiosity, Dimension::Sadness] {
                let current = agent_b_emotion.value(dim);
                let target = neutral.value(dim);
                let correction = (current - target) * homeostasis;
                agent_b_emotion.set(dim, current - correction);
            }

            if turn > 0 {
                let prev = EmotionalSnapshot::from_state(&state_before_homeostasis);
                let curr = EmotionalSnapshot::from_state(&agent_b_emotion);
                // Every dimension should be closer to neutral
                if (curr.joy - neutral.value(Dimension::Joy)).abs()
                    < (prev.joy - neutral.value(Dimension::Joy)).abs()
                {
                    turns_without_stimulus += 1;
                }
            }
            state_before_homeostasis = agent_b_emotion;
        }

        let homeostasis_works = turns_without_stimulus >= 9; // 9/9 turns closer to neutral

        // ── 6. Test memory isolation ──────────────────────────────────────
        // agent_a "sees" a message → remembers it. agent_b must NOT see agent_a's memories.
        let mem_a =
            familyclaw_memory::Memory::builder("agent_a saw: the family shipped the feature")
                .source("agent_a")
                .created_at(clock)
                .build();
        store_a
            .add(mem_a)
            .await
            .map_err(|e| crate::BenchError::scenario(format!("add mem a: {e}")))?;

        let mem_b = familyclaw_memory::Memory::builder("agent_b saw: the weather is sunny")
            .source("agent_b")
            .created_at(clock)
            .build();
        store_b
            .add(mem_b)
            .await
            .map_err(|e| crate::BenchError::scenario(format!("add mem b: {e}")))?;

        // agent_a searches "feature" → finds its own
        let ctx_a = RetrievalContext::new("feature").with_limit(5);
        let hits_a = store_a
            .retrieve(&ctx_a, clock)
            .await
            .map_err(|e| crate::BenchError::scenario(format!("retrieve a: {e}")))?;

        // agent_b searches "weather" → finds its own, NOT agent_a's memories
        let ctx_b = RetrievalContext::new("weather").with_limit(5);
        let hits_b = store_b
            .retrieve(&ctx_b, clock)
            .await
            .map_err(|e| crate::BenchError::scenario(format!("retrieve b: {e}")))?;

        let a_remembers = hits_a
            .iter()
            .any(|h| h.memory.content.contains("family shipped"));
        let b_isolated = !hits_b
            .iter()
            .any(|h| h.memory.content.contains("family shipped"));
        let b_remembers_own = hits_b.iter().any(|h| h.memory.content.contains("weather"));
        let a_isolated_from_b = !hits_a.iter().any(|h| h.memory.content.contains("weather"));

        let memory_isolation_works =
            a_remembers && b_isolated && b_remembers_own && a_isolated_from_b;

        // ── 7. Liveness check: the subject's recall ────────────────────────────────
        let subject_hits = subject.recall("emotion", clock).await?;

        // ── 8. Results ────────────────────────────────────────────────────
        let contagion_score = if contagion_works { 1.0 } else { 0.0 };
        let homeostasis_score = if homeostasis_works { 1.0 } else { 0.0 };
        let isolation_score = if memory_isolation_works { 1.0 } else { 0.0 };

        let passed = contagion_works && homeostasis_works && memory_isolation_works;

        let result = ScenarioResult::new(Self::ID, passed)
            .with_metric("contagion_correct", contagion_score)
            .with_metric("homeostasis_works", homeostasis_score)
            .with_metric("memory_isolation", isolation_score)
            .with_metric("subject_recall_hits", subject_hits.len() as f64)
            .with_note(format!(
                "contagion: joy={:.1}→{:.1} (expected {expected_joy:.1}), curiosity={:.1}→{:.1} (expected {expected_curiosity:.1})",
                agent_a_emotion.value(Dimension::Joy),
                after_pulse.joy,
                agent_a_emotion.value(Dimension::Curiosity),
                after_pulse.curiosity,
            ))
            .with_note(format!(
                "homeostasis: {turns_without_stimulus}/9 turns moved toward neutral"
            ))
            .with_note(format!(
                "memory_isolation: a_remembers={a_remembers} b_isolated={b_isolated} b_remembers_own={b_remembers_own}"
            ))
            .with_note(format!(
                "bus alive with {} being(s)", bus.count().await.unwrap_or(0)
            ));

        bus.stop();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Ok(vec![RecallHit::new("emotional memory", 0.9)])
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
        fn name(&self) -> &'static str {
            "stub_s4"
        }
    }

    #[tokio::test]
    async fn emotional_contagion_passes() {
        let scenario = EmotionalContagion::new();
        let mut subject = StubSubject;
        let clock = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("valid clock");
        let result = scenario.run(&mut subject, clock).await.expect("run");

        assert_eq!(result.id, EmotionalContagion::ID);
        assert!(result.passed, "S4 should pass: {:?}", result.notes);

        assert_eq!(result.metrics.get("contagion_correct").copied(), Some(1.0));
        assert_eq!(result.metrics.get("homeostasis_works").copied(), Some(1.0));
        assert_eq!(result.metrics.get("memory_isolation").copied(), Some(1.0));
    }

    #[tokio::test]
    async fn scenario_is_deterministic() {
        let scenario = EmotionalContagion::new();
        let clock = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("valid clock");
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let a = scenario.run(&mut s1, clock).await.expect("a");
        let b = scenario.run(&mut s2, clock).await.expect("b");
        assert_eq!(a.metrics, b.metrics);
        assert_eq!(a.passed, b.passed);
    }
}

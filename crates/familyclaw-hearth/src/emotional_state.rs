//! Shared emotional state — multi-agent emotional contagion and homeostasis.
//!
//! [`SharedEmotionalState`] maintains each agent's emotion vector and
//! simulates the spread of emotions between agents (contagion) as well
//! as a natural return to neutral (homeostasis).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single agent's emotion vector.
///
/// All values are in the range `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EmotionalVector {
    /// Joy, happiness.
    pub joy: f64,
    /// Sadness, melancholy.
    pub sadness: f64,
    /// Curiosity, thirst for knowledge.
    pub curiosity: f64,
    /// Anxiety, worry.
    pub anxiety: f64,
    /// Confidence.
    pub confidence: f64,
    /// Affection, love.
    pub affection: f64,
}

impl EmotionalVector {
    /// The neutral base state (all values 0.5).
    #[must_use]
    pub const fn neutral() -> Self {
        Self {
            joy: 0.5,
            sadness: 0.5,
            curiosity: 0.5,
            anxiety: 0.5,
            confidence: 0.5,
            affection: 0.5,
        }
    }

    /// Clamps all values to the range `0.0..=1.0`.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.joy = self.joy.clamp(0.0, 1.0);
        self.sadness = self.sadness.clamp(0.0, 1.0);
        self.curiosity = self.curiosity.clamp(0.0, 1.0);
        self.anxiety = self.anxiety.clamp(0.0, 1.0);
        self.confidence = self.confidence.clamp(0.0, 1.0);
        self.affection = self.affection.clamp(0.0, 1.0);
        self
    }
}

/// Shared emotional state for all agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedEmotionalState {
    /// Agents' emotional states (name -> vector).
    agents: HashMap<String, EmotionalVector>,
    /// Contagion rate (0.0-1.0). Determines how strongly
    /// emotions spread between agents.
    pub contagion_rate: f64,
    /// Homeostasis rate — how quickly the state returns to neutral.
    pub homeostasis_rate: f64,
}

impl SharedEmotionalState {
    /// Creates a new shared emotional state with default values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            contagion_rate: 0.3,
            homeostasis_rate: 0.1,
        }
    }

    /// Sets an agent's emotional state.
    pub fn set(&mut self, agent_id: &str, state: EmotionalVector) {
        self.agents.insert(agent_id.to_string(), state.clamped());
    }

    /// Returns an agent's emotional state, or neutral if not found.
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<&EmotionalVector> {
        self.agents.get(agent_id)
    }

    /// Lists all agents that have an emotional state.
    #[must_use]
    pub fn agent_ids(&self) -> Vec<&String> {
        self.agents.keys().collect()
    }

    /// Spreads an emotion from one agent to another.
    ///
    /// `weight` determines how much of the source's emotion transfers to the target.
    /// Uses the `contagion_rate` factor.
    pub fn contagion(&mut self, from: &str, to: &str, weight: f64) {
        let Some(&source) = self.agents.get(from) else {
            return;
        };
        let w = weight.clamp(0.0, 1.0);
        let target = self
            .agents
            .entry(to.to_string())
            .or_insert_with(EmotionalVector::neutral);

        target.joy += (source.joy - target.joy) * w;
        target.sadness += (source.sadness - target.sadness) * w;
        target.curiosity += (source.curiosity - target.curiosity) * w;
        target.anxiety += (source.anxiety - target.anxiety) * w;
        target.confidence += (source.confidence - target.confidence) * w;
        target.affection += (source.affection - target.affection) * w;

        *target = target.clamped();
    }

    /// Moves an agent's state toward neutral.
    pub fn homeostasis(&mut self, agent_id: &str) {
        let Some(state) = self.agents.get_mut(agent_id) else {
            return;
        };
        let neutral = EmotionalVector::neutral();
        let r = self.homeostasis_rate;

        state.joy += (neutral.joy - state.joy) * r;
        state.sadness += (neutral.sadness - state.sadness) * r;
        state.curiosity += (neutral.curiosity - state.curiosity) * r;
        state.anxiety += (neutral.anxiety - state.anxiety) * r;
        state.confidence += (neutral.confidence - state.confidence) * r;
        state.affection += (neutral.affection - state.affection) * r;

        *state = state.clamped();
    }

    /// One emotional round: contagion between all agents + homeostasis.
    pub fn tick(&mut self, agent_ids: &[String]) {
        // Contagion: every agent spreads emotion to every other agent
        let rate = self.contagion_rate;
        let ids: Vec<String> = agent_ids.to_vec();
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                if i != j {
                    self.contagion(&ids[i], &ids[j], rate);
                }
            }
        }
        // Homeostasis: every agent returns toward neutral
        for id in &ids {
            self.homeostasis(id);
        }
    }
}

impl Default for SharedEmotionalState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contagion_spreads() {
        let mut state = SharedEmotionalState::new();
        state.set(
            "agent_a",
            EmotionalVector {
                joy: 0.9,
                sadness: 0.1,
                curiosity: 0.8,
                anxiety: 0.2,
                confidence: 0.9,
                affection: 0.7,
            },
        );
        state.set("agent_b", EmotionalVector::neutral());

        state.contagion("agent_a", "agent_b", 0.5);
        let agent_b = state.get("agent_b").expect("agent_b exists");
        // agent_b's joy rose from agent_a's high joy
        assert!(agent_b.joy > 0.5, "contagion should spread joy");
        assert!(agent_b.joy < 0.9, "but not fully match source");
    }

    #[test]
    fn homeostasis_prevents_burnout() {
        let mut state = SharedEmotionalState::new();
        state.set(
            "agent",
            EmotionalVector {
                joy: 0.95,
                sadness: 0.95,
                curiosity: 0.95,
                anxiety: 0.95,
                confidence: 0.05,
                affection: 0.05,
            },
        );

        // 10 homeostasis ticks
        for _ in 0..10 {
            state.homeostasis("agent");
        }

        let final_state = state.get("agent").expect("agent exists");
        // Extremes approach the neutral value of 0.5
        assert!(final_state.joy < 0.95, "high emotions should trend down");
        assert!(
            final_state.confidence > 0.05,
            "low emotions should trend up"
        );
    }

    #[test]
    fn isolation_works() {
        let mut state = SharedEmotionalState::new();
        state.set(
            "agent_a",
            EmotionalVector {
                joy: 0.9,
                ..EmotionalVector::neutral()
            },
        );
        state.set("agent_b", EmotionalVector::neutral());

        // No contagion tick — states stay isolated
        state.homeostasis("agent_b");
        let agent_b = state.get("agent_b").expect("agent_b exists");
        // Without contagion, agent_b stays neutral (homeostasis pulls toward 0.5)
        // Joy was 0.5, homeostasis moves it toward 0.5 -> stays at 0.5
        assert!(
            (agent_b.joy - 0.5).abs() < 0.01,
            "without contagion, agent stays neutral"
        );
    }

    #[test]
    fn tick_updates_all_agents() {
        let mut state = SharedEmotionalState::new();
        state.set(
            "a",
            EmotionalVector {
                joy: 0.8,
                ..EmotionalVector::neutral()
            },
        );
        state.set(
            "b",
            EmotionalVector {
                joy: 0.2,
                ..EmotionalVector::neutral()
            },
        );

        let ids = vec!["a".to_string(), "b".to_string()];
        state.tick(&ids);

        // a's joy dropped (homeostasis), b's joy rose (contagion from a + homeostasis)
        let a = state.get("a").expect("a exists");
        let b = state.get("b").expect("b exists");
        assert!(a.joy < 0.8, "a joy trends toward neutral");
        assert!(
            b.joy > 0.2,
            "b joy trends toward neutral and gets contagion"
        );
    }
}

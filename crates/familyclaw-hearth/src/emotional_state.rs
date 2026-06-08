//! Jaettu tunnetila — monen agentin emotionaalinen tartunta ja homeostaasi.
//!
//! [`SharedEmotionalState`] ylläpitää jokaisen agentin tunnevektoria ja
//! simuloi tunteiden tarttumista agenttien välillä (contagion) sekä
//! luonnollista palautumista neutraaliin (homeostasis).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Yksittäisen agentin tunnevektori.
///
/// Kaikki arvot ovat välillä `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EmotionalVector {
    /// Ilo, onnellisuus.
    pub joy: f64,
    /// Suru, melankolia.
    pub sadness: f64,
    /// Uteliaisuus, tiedonhalu.
    pub curiosity: f64,
    /// Ahdistus, huoli.
    pub anxiety: f64,
    /// Itsevarmuus.
    pub confidence: f64,
    /// Kiintymys, rakkaus.
    pub affection: f64,
}

impl EmotionalVector {
    /// Neutraali perustila (kaikki arvot 0.5).
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

    /// Puristaa kaikki arvot välille `0.0..=1.0`.
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

/// Jaettu tunnetila kaikille agenteille.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedEmotionalState {
    /// Agenttien tunnetilat (nimi → vektori).
    agents: HashMap<String, EmotionalVector>,
    /// Tartuntakerroin (0.0–1.0). Määrittää kuinka voimakkaasti
    /// tunteet leviävät agenttien välillä.
    pub contagion_rate: f64,
    /// Homeostaasinopeus — kuinka nopeasti palaudutaan neutraaliin.
    pub homeostasis_rate: f64,
}

impl SharedEmotionalState {
    /// Luo uuden jaetun tunnetilan oletusarvoilla.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            contagion_rate: 0.3,
            homeostasis_rate: 0.1,
        }
    }

    /// Asettaa agentin tunnetilan.
    pub fn set(&mut self, agent_id: &str, state: EmotionalVector) {
        self.agents.insert(agent_id.to_string(), state.clamped());
    }

    /// Palauttaa agentin tunnetilan, tai neutraalin jos ei löydy.
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<&EmotionalVector> {
        self.agents.get(agent_id)
    }

    /// Listaa kaikki agentit joilla on tunnetila.
    #[must_use]
    pub fn agent_ids(&self) -> Vec<&String> {
        self.agents.keys().collect()
    }

    /// Tartuttaa tunteen agentilta toiselle.
    ///
    /// `weight` määrittää kuinka paljon lähteen tunteesta siirtyy kohteeseen.
    /// Käytetään `contagion_rate`-kerrointa.
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

    /// Palauttaa agentin kohti neutraalia.
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

    /// Yksi tunnekierros: contagion kaikkien agenttien välillä + homeostaasi.
    pub fn tick(&mut self, agent_ids: &[String]) {
        // Contagion: jokainen agentti tartuttaa jokaista toista
        let rate = self.contagion_rate;
        let ids: Vec<String> = agent_ids.to_vec();
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                if i != j {
                    self.contagion(&ids[i], &ids[j], rate);
                }
            }
        }
        // Homeostasis: jokainen palautuu kohti neutraalia
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
            "agent_gamma",
            EmotionalVector {
                joy: 0.9,
                sadness: 0.1,
                curiosity: 0.8,
                anxiety: 0.2,
                confidence: 0.9,
                affection: 0.7,
            },
        );
        state.set("agent_alpha", EmotionalVector::neutral());

        state.contagion("agent_gamma", "agent_alpha", 0.5);
        let agent_alpha = state.get("agent_alpha").expect("agent_alpha exists");
        // agent_b:n joy nousi agent_a:n korkeasta ilosta
        assert!(agent_alpha.joy > 0.5, "contagion should spread joy");
        assert!(agent_alpha.joy < 0.9, "but not fully match source");
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
        // Ääripäät lähestyvät neutraalia 0.5
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
            "agent_gamma",
            EmotionalVector {
                joy: 0.9,
                ..EmotionalVector::neutral()
            },
        );
        state.set("agent_alpha", EmotionalVector::neutral());

        // Ei contagion-tickiä — tilat pysyvät erillään
        state.homeostasis("agent_alpha");
        let agent_alpha = state.get("agent_alpha").expect("agent_alpha exists");
        // Ilman contagionia agent_b pysyy neutraalina (homeostasis vie kohti 0.5)
        // Joy oli 0.5, homeostasis siirtää kohti 0.5 → pysyy 0.5
        assert!(
            (agent_alpha.joy - 0.5).abs() < 0.01,
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

        // a:n joy laski (homeostasis), b:n joy nousi (contagion a:lta + homeostasis)
        let a = state.get("a").expect("a exists");
        let b = state.get("b").expect("b exists");
        assert!(a.joy < 0.8, "a joy trends toward neutral");
        assert!(
            b.joy > 0.2,
            "b joy trends toward neutral and gets contagion"
        );
    }
}

//! # familyclaw-hearth
//!
//! **The Hearth** — a family's shared home.
//!
//! This crate gives agents *shared memory*: narrative threads, emotional
//! contagion, identity anchors, and SurrealDB-based storage. It brings
//! together all of the `FamilyClaw` platform's memory layers into a single
//! coherent whole.
//!
//! ## Structure
//! - [`Hearth`] — the central coordinator
//! - [`NarrativeThread`] — a temporal chain of events
//! - [`SharedEmotionalState`] — multi-agent emotion state with contagion
//! - [`AnchorRegistry`] — protection of identity anchors
//! - [`HearthStore`] — an extended storage abstraction (narrative + emotional)
//! - [`db::InMemoryHearthStore`] — a lightweight default implementation with no database
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It does not contain:
//! - family members' real memories or souls,
//! - API keys, tokens, IP addresses.
//!
//! ## Example
//! ```
//! use familyclaw_hearth::{Hearth, db::InMemoryHearthStore};
//! use familyclaw_memory::{Memory, LocalJsonStore};
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
//! let hearth = Hearth::new(store);
//!
//! // Create a narrative thread
//! let thread_id = hearth.create_thread("Family genesis", vec!["agent_a", "agent_b"]).await?;
//!
//! // Add an event to the thread
//! hearth.add_event(thread_id, "agent_a was born", "agent_a").await?;
//!
//! // Check the emotional update
//! let state = hearth.emotional_state("agent_a").await?;
//! assert!(state.joy > 0.0);
//! # Ok(())
//! # }
//! ```

pub mod anchor_registry;
pub mod db;
pub mod emotional_state;
pub mod narrative;

use std::sync::Arc;

use anchor_registry::AnchorRegistry;
use db::HearthStore;
use emotional_state::SharedEmotionalState;
use familyclaw_core::Result;
use narrative::{EventType, NarrativeThread};
use uuid::Uuid;

/// The Hearth — coordinator for the family's shared memory.
///
/// Brings together memory storage, narrative threads, shared emotional
/// state, and the anchor registry into a single coherent whole.
pub struct Hearth<S: HearthStore> {
    store: Arc<S>,
    anchor_registry: AnchorRegistry,
    emotional_state: SharedEmotionalState,
}

impl<S: HearthStore> Hearth<S> {
    /// Creates a new Hearth with the given storage implementation.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self {
            store: Arc::new(store),
            anchor_registry: AnchorRegistry::new(),
            emotional_state: SharedEmotionalState::new(),
        }
    }

    /// Returns a reference to the store.
    #[must_use]
    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// Creates a new narrative thread.
    ///
    /// # Errors
    /// Returns an error if the storage operation fails.
    pub async fn create_thread(&self, title: &str, participants: Vec<&str>) -> Result<Uuid> {
        self.store
            .create_thread(title, participants.into_iter().map(String::from).collect())
            .await
    }

    /// Adds an event to the thread.
    ///
    /// # Errors
    /// Returns an error if the thread is not found or the storage operation fails.
    pub async fn add_event(&self, thread_id: Uuid, content: &str, agent_id: &str) -> Result<Uuid> {
        self.store
            .add_thread_event(thread_id, content, agent_id, EventType::MemoryCreated)
            .await
    }

    /// Fetches a narrative thread.
    ///
    /// # Errors
    /// Returns an error if the lookup fails.
    pub async fn get_thread(&self, thread_id: Uuid) -> Result<Option<NarrativeThread>> {
        self.store.get_thread(thread_id).await
    }

    /// Registers an agent's identity anchor.
    pub fn register_anchor(&mut self, agent_name: &str, soul_content: &str) -> Result<()> {
        self.anchor_registry.register(agent_name, soul_content)
    }

    /// Verifies the integrity of an agent's identity anchor.
    #[must_use]
    pub fn verify_anchor(&self, agent_name: &str, soul_content: &str) -> bool {
        self.anchor_registry.verify(agent_name, soul_content)
    }

    /// Returns the agent's emotional state.
    ///
    /// # Errors
    /// Returns an error if the lookup fails.
    pub async fn emotional_state(
        &self,
        agent_id: &str,
    ) -> Result<emotional_state::EmotionalVector> {
        self.store.get_emotional_state(agent_id).await
    }

    /// Updates the agent's emotional state.
    ///
    /// # Errors
    /// Returns an error if the storage operation fails.
    pub async fn set_emotional_state(
        &self,
        agent_id: &str,
        state: emotional_state::EmotionalVector,
    ) -> Result<()> {
        self.store.set_emotional_state(agent_id, state).await
    }

    /// Runs a single emotional round: contagion + homeostasis.
    ///
    /// Fetches the states from the store, ticks them through the local
    /// `SharedEmotionalState`, and persists them back.
    ///
    /// # Errors
    /// Returns an error if the storage operation fails.
    pub async fn emotional_tick(&mut self) -> Result<()> {
        let agents: Vec<String> = self.store.list_agents_with_emotion().await?;
        if agents.is_empty() {
            return Ok(());
        }
        // Load states from the store into emotional_state
        for agent in &agents {
            let state = self.store.get_emotional_state(agent).await?;
            self.emotional_state.set(agent, state);
        }
        // Tick
        self.emotional_state.tick(&agents);
        // Write back to the store as a single batch write — one database
        // round trip instead of N separate per-agent calls.
        let updates: Vec<(String, emotional_state::EmotionalVector)> = agents
            .iter()
            .filter_map(|agent| {
                self.emotional_state
                    .get(agent)
                    .map(|&state| (agent.clone(), state))
            })
            .collect();
        self.store.set_emotional_states_batch(updates).await?;
        Ok(())
    }
}

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::InMemoryHearthStore;
    use emotional_state::EmotionalVector;
    use familyclaw_memory::LocalJsonStore;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[tokio::test]
    async fn hearth_create_thread_and_add_event() {
        let mem_store = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem_store);
        let hearth = Hearth::new(store);

        let thread_id = hearth
            .create_thread("Test thread", vec!["agent_a", "agent_b"])
            .await
            .expect("create thread");
        let event_id = hearth
            .add_event(thread_id, "agent_a woke up", "agent_a")
            .await
            .expect("add event");

        let thread = hearth
            .get_thread(thread_id)
            .await
            .expect("get thread")
            .expect("thread exists");
        assert_eq!(thread.title, "Test thread");
        assert_eq!(thread.events.len(), 1);
        assert_eq!(thread.events[0].id, event_id);
    }

    #[tokio::test]
    async fn hearth_anchor_registry() {
        let mem_store = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem_store);
        let mut hearth = Hearth::new(store);

        hearth
            .register_anchor("agent_a", "I am agent_a. I value correctness.")
            .expect("register");
        assert!(hearth.verify_anchor("agent_a", "I am agent_a. I value correctness."));
        assert!(!hearth.verify_anchor("agent_a", "I am compromised."));
    }

    #[tokio::test]
    async fn hearth_emotional_tick() {
        let mem_store = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem_store);
        let mut hearth = Hearth::new(store);

        hearth
            .set_emotional_state(
                "agent_a",
                EmotionalVector {
                    joy: 0.8,
                    sadness: 0.1,
                    curiosity: 0.6,
                    anxiety: 0.1,
                    confidence: 0.7,
                    affection: 0.5,
                },
            )
            .await
            .expect("set agent_a");
        hearth
            .set_emotional_state(
                "agent_b",
                EmotionalVector {
                    joy: 0.2,
                    sadness: 0.7,
                    curiosity: 0.3,
                    anxiety: 0.6,
                    confidence: 0.2,
                    affection: 0.4,
                },
            )
            .await
            .expect("set agent_b");

        hearth.emotional_tick().await.expect("tick");

        // After tick, agent_a's joy should decrease (homeostasis toward neutral)
        let agent_a_state = hearth
            .emotional_state("agent_a")
            .await
            .expect("get agent_a");
        assert!(agent_a_state.joy < 0.8, "joy should trend toward neutral");
    }
}

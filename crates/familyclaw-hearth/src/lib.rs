//! # familyclaw-hearth
//!
//! **The Hearth** — perheen jaettu koti.
//!
//! Tämä crate antaa agenteille *jaetun muistin*: narratiiviset langat,
//! emotionaalisen tartunnan, identiteetti-ankkurit ja SurrealDB-pohjaisen
//! tallennuksen. Se kokoaa kaikki FamilyClaw-alustan muistikerrokset yhdeksi
//! eheäksi kokonaisuudeksi.
//!
//! ## Rakenne
//! - [`Hearth`] — keskitetty koordinaattori
//! - [`NarrativeThread`] — tapahtumien ajallinen ketju
//! - [`SharedEmotionalState`] — monen agentin tunnetila tartunnalla
//! - [`AnchorRegistry`] — identiteetti-ankkurien suojaus
//! - [`HearthStore`] — laajennettu tallennusabstraktio (narrative + emotional)
//! - [`InMemoryHearthStore`] — kevyt oletustoteutus ilman tietokantaa
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se ei sisällä:
//! - perheenjäsenten oikeita muistoja tai sieluja,
//! - API-avaimia, tokeneita, IP-osoitteita.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_hearth::{Hearth, db::InMemoryHearthStore};
//! use familyclaw_memory::{Memory, LocalJsonStore};
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
//! let hearth = Hearth::new(store);
//!
//! // Luo narratiivinen lanka
//! let thread_id = hearth.create_thread("Family genesis", vec!["agent_a", "agent_b"]).await?;
//!
//! // Lisää tapahtuma lankaan
//! hearth.add_event(thread_id, "agent_a was born", "agent_a").await?;
//!
//! // Tarkista tunnepäivitys
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

/// The Hearth — perheen jaetun muistin koordinaattori.
///
/// Yhdistää muistitallennuksen, narratiiviset langat, jaetun tunnetilan
/// ja ankkurirekisterin yhdeksi eheäksi kokonaisuudeksi.
pub struct Hearth<S: HearthStore> {
    store: Arc<S>,
    anchor_registry: AnchorRegistry,
    emotional_state: SharedEmotionalState,
}

impl<S: HearthStore> Hearth<S> {
    /// Luo uusi Hearth annetulla tallennustoteutuksella.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self {
            store: Arc::new(store),
            anchor_registry: AnchorRegistry::new(),
            emotional_state: SharedEmotionalState::new(),
        }
    }

    /// Palauttaa viitteen tallennukseen.
    #[must_use]
    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// Luo uuden narratiivisen langan.
    ///
    /// # Errors
    /// Palauttaa virheen jos tallennus epäonnistuu.
    pub async fn create_thread(&self, title: &str, participants: Vec<&str>) -> Result<Uuid> {
        self.store
            .create_thread(title, participants.into_iter().map(String::from).collect())
            .await
    }

    /// Lisää tapahtuman lankaan.
    ///
    /// # Errors
    /// Palauttaa virheen jos lankaa ei löydy tai tallennus epäonnistuu.
    pub async fn add_event(&self, thread_id: Uuid, content: &str, agent_id: &str) -> Result<Uuid> {
        self.store
            .add_thread_event(thread_id, content, agent_id, EventType::MemoryCreated)
            .await
    }

    /// Hakee narratiivisen langan.
    ///
    /// # Errors
    /// Palauttaa virheen jos haku epäonnistuu.
    pub async fn get_thread(&self, thread_id: Uuid) -> Result<Option<NarrativeThread>> {
        self.store.get_thread(thread_id).await
    }

    /// Rekisteröi agentin identiteetti-ankkurin.
    pub fn register_anchor(&mut self, agent_name: &str, soul_content: &str) -> Result<()> {
        self.anchor_registry.register(agent_name, soul_content)
    }

    /// Tarkistaa agentin identiteetti-ankkurin eheyden.
    #[must_use]
    pub fn verify_anchor(&self, agent_name: &str, soul_content: &str) -> bool {
        self.anchor_registry.verify(agent_name, soul_content)
    }

    /// Palauttaa agentin tunnetilan.
    ///
    /// # Errors
    /// Palauttaa virheen jos haku epäonnistuu.
    pub async fn emotional_state(
        &self,
        agent_id: &str,
    ) -> Result<emotional_state::EmotionalVector> {
        self.store.get_emotional_state(agent_id).await
    }

    /// Päivittää agentin tunnetilan.
    ///
    /// # Errors
    /// Palauttaa virheen jos tallennus epäonnistuu.
    pub async fn set_emotional_state(
        &self,
        agent_id: &str,
        state: emotional_state::EmotionalVector,
    ) -> Result<()> {
        self.store.set_emotional_state(agent_id, state).await
    }

    /// Suorittaa yhden tunnekierroksen: contagion + homeostaasi.
    ///
    /// Hakee tilat storesta, tickkaa paikallisen SharedEmotionalState:n
    /// läpi, ja persistoi takaisin.
    ///
    /// # Errors
    /// Palauttaa virheen jos tallennus epäonnistuu.
    pub async fn emotional_tick(&mut self) -> Result<()> {
        let agents: Vec<String> = self.store.list_agents_with_emotion().await?;
        if agents.is_empty() {
            return Ok(());
        }
        // Lue tilat storesta emotional_stateen
        for agent in &agents {
            let state = self.store.get_emotional_state(agent).await?;
            self.emotional_state.set(agent, state);
        }
        // Tick
        self.emotional_state.tick(&agents);
        // Kirjoita takaisin storeen.
        // TODO(perf): batch-kirjoitus — yksi query joka päivittää kaikki
        // tunnetilat kerralla per-agentti-kutsujen sijaan (N round-trippiä).
        for agent in &agents {
            if let Some(&state) = self.emotional_state.get(agent) {
                self.store.set_emotional_state(agent, state).await?;
            }
        }
        Ok(())
    }
}

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
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

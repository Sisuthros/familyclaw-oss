//! Tietokantakerros — [`HearthStore`]-trait ja [`InMemoryHearthStore`]-toteutus.
//!
//! [`HearthStore`] laajentaa [`familyclaw_memory::MemoryStore`]-traittia
//! narratiivisilla langoilla, jaetulla tunnetilalla ja ankkurituella.
//! [`InMemoryHearthStore`] on kevyt oletustoteutus joka käärii
//! minkä tahansa `MemoryStore`-toteutuksen.

pub mod schema;
#[cfg(feature = "surreal")]
pub mod surreal;

use std::collections::HashMap;

use familyclaw_core::Result;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::emotional_state::EmotionalVector;
use crate::narrative::{EventType, NarrativeThread, ThreadEvent};

/// Delegoi `MemoryStore`-metodikutsun kääritylle `self.memory`-toteutukselle.
///
/// Poistaa toistuvan `self.memory.<method>(<args>)`-rungon
/// [`InMemoryHearthStore`]:n `MemoryStore`-toteutuksesta.
macro_rules! delegate_memory_store {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        $self.memory.$method($($arg),*)
    };
}

/// Laajennettu tallennusabstraktio Hearthille.
///
/// Laajentaa [`familyclaw_memory::MemoryStore`]:n narratiivisilla
/// langoilla ja jaetulla tunnetilalla.
pub trait HearthStore: familyclaw_memory::MemoryStore {
    // --- Narrative threads ---

    /// Hakee narratiivisen langan.
    fn get_thread(
        &self,
        thread_id: Uuid,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<NarrativeThread>>> + Send + '_>,
    >;

    /// Tallentaa (luo tai korvaa) narratiivisen langan kokonaisuudessaan.
    ///
    /// Tämä on alkeismetodi jonka päälle [`HearthStore::create_thread`] ja
    /// [`HearthStore::add_thread_event`] rakentuvat (luku–muokkaus–kirjoitus).
    fn set_thread(
        &self,
        thread: NarrativeThread,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    >;

    /// Luo uuden narratiivisen langan.
    ///
    /// Default-toteutus rakentuu [`HearthStore::set_thread`]:n päälle; toteuttaja
    /// voi ohittaa sen tehokkaammalla versiolla.
    fn create_thread(
        &self,
        title: &str,
        participants: Vec<String>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>,
    > {
        let thread = NarrativeThread::new(title, participants);
        Box::pin(async move {
            let id = thread.id;
            self.set_thread(thread).await?;
            Ok(id)
        })
    }

    /// Lisää tapahtuman lankaan.
    ///
    /// Default-toteutus tekee luku–muokkaus–kirjoitus-syklin
    /// [`HearthStore::get_thread`]:n ja [`HearthStore::set_thread`]:n kautta;
    /// toteuttaja voi ohittaa sen tehokkaammalla versiolla.
    fn add_thread_event(
        &self,
        thread_id: Uuid,
        content: &str,
        agent_id: &str,
        event_type: EventType,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>,
    > {
        let event = ThreadEvent::new(thread_id, event_type, content, agent_id);
        Box::pin(async move {
            let Some(mut thread) = self.get_thread(thread_id).await? else {
                return Err(familyclaw_core::FamilyClawError::Memory(format!(
                    "thread {thread_id} not found"
                )));
            };
            let event_id = event.id;
            thread.add_event(event);
            self.set_thread(thread).await?;
            Ok(event_id)
        })
    }

    // --- Emotional state ---

    /// Hakee agentin tunnetilan.
    fn get_emotional_state(
        &self,
        agent_id: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EmotionalVector>> + Send + '_>,
    >;

    /// Asettaa agentin tunnetilan.
    fn set_emotional_state(
        &self,
        agent_id: &str,
        state: EmotionalVector,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    >;

    /// Listaa kaikki agentit joilla on tunnetila.
    fn list_agents_with_emotion(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>>> + Send + '_>,
    >;
}

/// Kevyt muistinvarainen toteutus — käärii minkä tahansa `MemoryStore`:n.
///
/// Pitää narratiiviset langat ja tunnetilat muistissa `RwLock`-suojattuna.
/// Soveltuu kehitykseen ja testaukseen; tuotantokäyttöön suositellaan
/// SurrealDB-pohjaista toteutusta.
pub struct InMemoryHearthStore<M: familyclaw_memory::MemoryStore> {
    /// Kääritty MemoryStore-toteutus.
    memory: M,
    /// Narratiiviset langat (thread_id → thread).
    threads: RwLock<HashMap<Uuid, NarrativeThread>>,
    /// Agenttien tunnetilat (agent_id → state).
    emotional_states: RwLock<HashMap<String, EmotionalVector>>,
}

impl<M: familyclaw_memory::MemoryStore> InMemoryHearthStore<M> {
    /// Luo uuden InMemoryHearthStore:n annetulla MemoryStore-toteutuksella.
    #[must_use]
    pub fn new(memory: M) -> Self {
        Self {
            memory,
            threads: RwLock::new(HashMap::new()),
            emotional_states: RwLock::new(HashMap::new()),
        }
    }
}

impl<M: familyclaw_memory::MemoryStore> familyclaw_memory::MemoryStore
    for InMemoryHearthStore<M>
{
    fn add(
        &self,
        memory: familyclaw_memory::Memory,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<familyclaw_core::MessageId>>
                + Send
                + '_,
        >,
    > {
        delegate_memory_store!(self, add, memory)
    }

    fn get(
        &self,
        id: familyclaw_core::MessageId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Option<familyclaw_memory::Memory>>,
                > + Send
                + '_,
        >,
    > {
        delegate_memory_store!(self, get, id)
    }

    fn update(
        &self,
        memory: familyclaw_memory::Memory,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    > {
        delegate_memory_store!(self, update, memory)
    }

    fn reinforce(
        &self,
        id: familyclaw_core::MessageId,
        at: familyclaw_core::Timestamp,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    > {
        delegate_memory_store!(self, reinforce, id, at)
    }

    fn set_status(
        &self,
        id: familyclaw_core::MessageId,
        status: familyclaw_memory::MemoryStatus,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    > {
        delegate_memory_store!(self, set_status, id, status)
    }

    fn all(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<familyclaw_memory::Memory>>>
                + Send
                + '_,
        >,
    > {
        delegate_memory_store!(self, all)
    }

    fn len(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>,
    > {
        delegate_memory_store!(self, len)
    }

    fn is_empty(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>,
    > {
        delegate_memory_store!(self, is_empty)
    }

    fn retrieve(
        &self,
        ctx: &familyclaw_memory::RetrievalContext,
        at: familyclaw_core::Timestamp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Vec<familyclaw_memory::RetrievalResult>>,
                > + Send
                + '_,
        >,
    > {
        delegate_memory_store!(self, retrieve, ctx, at)
    }

    fn run_decay(
        &self,
        thresholds: familyclaw_memory::DecayThresholds,
        at: familyclaw_core::Timestamp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<familyclaw_memory::DecayReport>,
                > + Send
                + '_,
        >,
    > {
        delegate_memory_store!(self, run_decay, thresholds, at)
    }
}

impl<M: familyclaw_memory::MemoryStore + Send + Sync> HearthStore
    for InMemoryHearthStore<M>
{
    fn create_thread(
        &self,
        title: &str,
        participants: Vec<String>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>,
    > {
        let title = title.to_string();
        Box::pin(async move {
            let thread = NarrativeThread::new(&title, participants);
            let id = thread.id;
            self.threads.write().await.insert(id, thread);
            Ok(id)
        })
    }

    fn add_thread_event(
        &self,
        thread_id: Uuid,
        content: &str,
        agent_id: &str,
        event_type: EventType,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>,
    > {
        let content = content.to_string();
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let event =
                ThreadEvent::new(thread_id, event_type, &content, &agent_id);
            let event_id = event.id;
            let mut threads = self.threads.write().await;
            let thread = threads
                .get_mut(&thread_id)
                .ok_or_else(|| familyclaw_core::FamilyClawError::Memory(
                    format!("thread {thread_id} not found")
                ))?;
            thread.add_event(event);
            Ok(event_id)
        })
    }

    fn get_thread(
        &self,
        thread_id: Uuid,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<NarrativeThread>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let threads = self.threads.read().await;
            Ok(threads.get(&thread_id).cloned())
        })
    }

    fn set_thread(
        &self,
        thread: NarrativeThread,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    > {
        Box::pin(async move {
            self.threads.write().await.insert(thread.id, thread);
            Ok(())
        })
    }

    fn get_emotional_state(
        &self,
        agent_id: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EmotionalVector>> + Send + '_>,
    > {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let states = self.emotional_states.read().await;
            Ok(states
                .get(&agent_id)
                .copied()
                .unwrap_or_else(EmotionalVector::neutral))
        })
    }

    fn set_emotional_state(
        &self,
        agent_id: &str,
        state: EmotionalVector,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<()>> + Send + '_>,
    > {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            self.emotional_states
                .write()
                .await
                .insert(agent_id, state.clamped());
            Ok(())
        })
    }

    fn list_agents_with_emotion(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>>> + Send + '_>,
    > {
        Box::pin(async move {
            let states = self.emotional_states.read().await;
            Ok(states.keys().cloned().collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_memory::LocalJsonStore;

    #[tokio::test]
    async fn store_and_retrieve_narrative_thread() {
        let mem = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem);

        let thread_id = HearthStore::create_thread(
            &store,
            "Test",
            vec!["a".into()],
        )
        .await
        .expect("create");
        HearthStore::add_thread_event(
            &store,
            thread_id,
            "hello",
            "a",
            EventType::MemoryCreated,
        )
        .await
        .expect("add event");

        let thread = HearthStore::get_thread(&store, thread_id)
            .await
            .expect("get thread")
            .expect("exists");
        assert_eq!(thread.title, "Test");
        assert_eq!(thread.events.len(), 1);
    }

    #[tokio::test]
    async fn emotional_state_roundtrip() {
        let mem = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem);

        let state = EmotionalVector {
            joy: 0.8,
            ..EmotionalVector::neutral()
        };
        HearthStore::set_emotional_state(&store, "agent_gamma", state)
            .await
            .expect("set");
        let got = HearthStore::get_emotional_state(&store, "agent_gamma")
            .await
            .expect("get");
        assert!((got.joy - 0.8).abs() < f64::EPSILON);

        let agents = HearthStore::list_agents_with_emotion(&store)
            .await
            .expect("list");
        assert!(agents.contains(&"agent_gamma".to_string()));
    }

    #[tokio::test]
    async fn delegates_to_memory_store() {
        let mem = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem);

        assert!(familyclaw_memory::MemoryStore::is_empty(&store)
            .await
            .expect("is_empty"));

        let m = familyclaw_memory::Memory::builder("test")
            .build();
        let _id = familyclaw_memory::MemoryStore::add(&store, m)
            .await
            .expect("add");

        assert!(!familyclaw_memory::MemoryStore::is_empty(&store)
            .await
            .expect("is_empty"));
    }
}

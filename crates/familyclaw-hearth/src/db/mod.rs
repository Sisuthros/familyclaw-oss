//! Database layer — the [`HearthStore`] trait and the [`InMemoryHearthStore`] implementation.
//!
//! [`HearthStore`] extends the [`familyclaw_memory::MemoryStore`] trait
//! with narrative threads, shared emotional state, and anchor support.
//! [`InMemoryHearthStore`] is a lightweight default implementation that wraps
//! any `MemoryStore` implementation.

pub mod schema;
#[cfg(feature = "surreal")]
pub mod surreal;

use std::collections::HashMap;

use familyclaw_core::Result;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::emotional_state::EmotionalVector;
use crate::narrative::{EventType, NarrativeThread, ThreadEvent};

/// Delegates a `MemoryStore` method call to the wrapped `self.memory` implementation.
///
/// Removes the repetitive `self.memory.<method>(<args>)` boilerplate from
/// [`InMemoryHearthStore`]'s `MemoryStore` implementation.
macro_rules! delegate_memory_store {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        $self.memory.$method($($arg),*)
    };
}

/// Extended storage abstraction for the Hearth.
///
/// Extends [`familyclaw_memory::MemoryStore`] with narrative threads
/// and shared emotional state.
pub trait HearthStore: familyclaw_memory::MemoryStore {
    // --- Narrative threads ---

    /// Fetches a narrative thread.
    fn get_thread(
        &self,
        thread_id: Uuid,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<NarrativeThread>>> + Send + '_>,
    >;

    /// Stores (creates or replaces) a narrative thread in its entirety.
    ///
    /// This is the primitive method on top of which [`HearthStore::create_thread`]
    /// and [`HearthStore::add_thread_event`] are built (read-modify-write).
    fn set_thread(
        &self,
        thread: NarrativeThread,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;

    /// Creates a new narrative thread.
    ///
    /// The default implementation is built on top of [`HearthStore::set_thread`];
    /// implementers may override it with a more efficient version.
    fn create_thread(
        &self,
        title: &str,
        participants: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
        let thread = NarrativeThread::new(title, participants);
        Box::pin(async move {
            let id = thread.id;
            self.set_thread(thread).await?;
            Ok(id)
        })
    }

    /// Adds an event to the thread.
    ///
    /// The default implementation performs a read-modify-write cycle via
    /// [`HearthStore::get_thread`] and [`HearthStore::set_thread`];
    /// implementers may override it with a more efficient version.
    fn add_thread_event(
        &self,
        thread_id: Uuid,
        content: &str,
        agent_id: &str,
        event_type: EventType,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
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

    /// Fetches an agent's emotional state.
    fn get_emotional_state(
        &self,
        agent_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<EmotionalVector>> + Send + '_>>;

    /// Sets an agent's emotional state.
    fn set_emotional_state(
        &self,
        agent_id: &str,
        state: EmotionalVector,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;

    /// Sets emotional states for multiple agents at once (batch).
    ///
    /// This is semantically identical to calling
    /// [`HearthStore::set_emotional_state`] for each `(agent_id, state)`
    /// pair — same end state — but implementers may persist them in
    /// **a single database round trip** (one transaction / one query)
    /// instead of N separate ones.
    ///
    /// The default implementation delegates to the per-agent calls so that
    /// existing [`HearthStore`] implementations keep working unchanged; an
    /// efficient backend (e.g. `SurrealDB`) overrides this with a bundled
    /// version.
    ///
    /// # Errors
    /// Returns an error if storing any of the states fails.
    /// On error, some states may already have been written (same as
    /// in the per-agent loop).
    fn set_emotional_states_batch(
        &self,
        states: Vec<(String, EmotionalVector)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            for (agent_id, state) in states {
                self.set_emotional_state(&agent_id, state).await?;
            }
            Ok(())
        })
    }

    /// Lists all agents that have an emotional state.
    fn list_agents_with_emotion(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>>> + Send + '_>>;
}

/// Lightweight in-memory implementation — wraps any `MemoryStore`.
///
/// Keeps narrative threads and emotional states in memory, guarded by an
/// `RwLock`. Suitable for development and testing; a SurrealDB-backed
/// implementation is recommended for production use.
pub struct InMemoryHearthStore<M: familyclaw_memory::MemoryStore> {
    /// The wrapped `MemoryStore` implementation.
    memory: M,
    /// Narrative threads (`thread_id` -> thread).
    threads: RwLock<HashMap<Uuid, NarrativeThread>>,
    /// Agents' emotional states (`agent_id` -> state).
    emotional_states: RwLock<HashMap<String, EmotionalVector>>,
}

impl<M: familyclaw_memory::MemoryStore> InMemoryHearthStore<M> {
    /// Creates a new `InMemoryHearthStore` with the given `MemoryStore` implementation.
    #[must_use]
    pub fn new(memory: M) -> Self {
        Self {
            memory,
            threads: RwLock::new(HashMap::new()),
            emotional_states: RwLock::new(HashMap::new()),
        }
    }
}

impl<M: familyclaw_memory::MemoryStore> familyclaw_memory::MemoryStore for InMemoryHearthStore<M> {
    fn add(
        &self,
        memory: familyclaw_memory::Memory,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<familyclaw_core::MessageId>> + Send + '_>,
    > {
        delegate_memory_store!(self, add, memory)
    }

    fn get(
        &self,
        id: familyclaw_core::MessageId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<familyclaw_memory::Memory>>> + Send + '_,
        >,
    > {
        delegate_memory_store!(self, get, id)
    }

    fn update(
        &self,
        memory: familyclaw_memory::Memory,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        delegate_memory_store!(self, update, memory)
    }

    fn reinforce(
        &self,
        id: familyclaw_core::MessageId,
        at: familyclaw_core::Timestamp,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        delegate_memory_store!(self, reinforce, id, at)
    }

    fn set_status(
        &self,
        id: familyclaw_core::MessageId,
        status: familyclaw_memory::MemoryStatus,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        delegate_memory_store!(self, set_status, id, status)
    }

    fn all(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<familyclaw_memory::Memory>>> + Send + '_>,
    > {
        delegate_memory_store!(self, all)
    }

    fn len(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>> {
        delegate_memory_store!(self, len)
    }

    fn is_empty(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>> {
        delegate_memory_store!(self, is_empty)
    }

    fn retrieve(
        &self,
        ctx: &familyclaw_memory::RetrievalContext,
        at: familyclaw_core::Timestamp,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<familyclaw_memory::RetrievalResult>>>
                + Send
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
        Box<dyn std::future::Future<Output = Result<familyclaw_memory::DecayReport>> + Send + '_>,
    > {
        delegate_memory_store!(self, run_decay, thresholds, at)
    }
}

impl<M: familyclaw_memory::MemoryStore + Send + Sync> HearthStore for InMemoryHearthStore<M> {
    fn create_thread(
        &self,
        title: &str,
        participants: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid>> + Send + '_>> {
        let content = content.to_string();
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let event = ThreadEvent::new(thread_id, event_type, &content, &agent_id);
            let event_id = event.id;
            let mut threads = self.threads.write().await;
            let thread = threads.get_mut(&thread_id).ok_or_else(|| {
                familyclaw_core::FamilyClawError::Memory(format!("thread {thread_id} not found"))
            })?;
            thread.add_event(event);
            Ok(event_id)
        })
    }

    fn get_thread(
        &self,
        thread_id: Uuid,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<NarrativeThread>>> + Send + '_>,
    > {
        Box::pin(async move {
            let threads = self.threads.read().await;
            Ok(threads.get(&thread_id).cloned())
        })
    }

    fn set_thread(
        &self,
        thread: NarrativeThread,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.threads.write().await.insert(thread.id, thread);
            Ok(())
        })
    }

    fn get_emotional_state(
        &self,
        agent_id: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<EmotionalVector>> + Send + '_>>
    {
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
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            self.emotional_states
                .write()
                .await
                .insert(agent_id, state.clamped());
            Ok(())
        })
    }

    fn set_emotional_states_batch(
        &self,
        states: Vec<(String, EmotionalVector)>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Take the lock once for the whole batch instead of per-agent locking.
            let mut guard = self.emotional_states.write().await;
            for (agent_id, state) in states {
                guard.insert(agent_id, state.clamped());
            }
            Ok(())
        })
    }

    fn list_agents_with_emotion(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>>> + Send + '_>> {
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

        let thread_id = HearthStore::create_thread(&store, "Test", vec!["a".into()])
            .await
            .expect("create");
        HearthStore::add_thread_event(&store, thread_id, "hello", "a", EventType::MemoryCreated)
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
        HearthStore::set_emotional_state(&store, "agent_a", state)
            .await
            .expect("set");
        let got = HearthStore::get_emotional_state(&store, "agent_a")
            .await
            .expect("get");
        assert!((got.joy - 0.8).abs() < f64::EPSILON);

        let agents = HearthStore::list_agents_with_emotion(&store)
            .await
            .expect("list");
        assert!(agents.contains(&"agent_a".to_string()));
    }

    /// A batch update produces the same end state as per-agent calls.
    #[tokio::test]
    async fn batch_equals_per_agent_updates() {
        let states = vec![
            (
                "agent_a".to_string(),
                EmotionalVector {
                    joy: 0.8,
                    sadness: 0.1,
                    curiosity: 0.6,
                    anxiety: 0.2,
                    confidence: 0.7,
                    affection: 0.5,
                },
            ),
            (
                "agent_b".to_string(),
                EmotionalVector {
                    joy: 0.2,
                    sadness: 0.7,
                    curiosity: 0.3,
                    anxiety: 0.6,
                    confidence: 0.2,
                    affection: 0.4,
                },
            ),
            (
                "agent_c".to_string(),
                EmotionalVector {
                    joy: 0.5,
                    sadness: 0.5,
                    curiosity: 0.9,
                    anxiety: 0.1,
                    confidence: 0.5,
                    affection: 0.6,
                },
            ),
        ];

        // Store 1: per-agent (reference).
        let per_agent = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        for (agent, state) in &states {
            HearthStore::set_emotional_state(&per_agent, agent, *state)
                .await
                .expect("set per-agent");
        }

        // Store 2: batch.

        let batch = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        HearthStore::set_emotional_states_batch(&batch, states.clone())
            .await
            .expect("set batch");

        // Both stores must have an identical state for every agent.
        for (agent, _) in &states {
            let a = HearthStore::get_emotional_state(&per_agent, agent)
                .await
                .expect("get per-agent");
            let b = HearthStore::get_emotional_state(&batch, agent)
                .await
                .expect("get batch");
            assert_eq!(a, b, "state mismatch for {agent}");
        }

        // Both stores must have the same set of agents.
        let mut agents_a = HearthStore::list_agents_with_emotion(&per_agent)
            .await
            .expect("list per-agent");
        let mut agents_b = HearthStore::list_agents_with_emotion(&batch)
            .await
            .expect("list batch");
        agents_a.sort();
        agents_b.sort();
        assert_eq!(agents_a, agents_b);
    }

    /// Edge case: 0 agents — no-op, no error, no rows.
    #[tokio::test]
    async fn batch_empty_is_noop() {
        let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        HearthStore::set_emotional_states_batch(&store, vec![])
            .await
            .expect("empty batch");
        let agents = HearthStore::list_agents_with_emotion(&store)
            .await
            .expect("list");
        assert!(agents.is_empty());
    }

    /// Edge case: 1 agent.
    #[tokio::test]
    async fn batch_single_agent() {
        let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        let state = EmotionalVector {
            joy: 0.9,
            ..EmotionalVector::neutral()
        };
        HearthStore::set_emotional_states_batch(&store, vec![("solo".to_string(), state)])
            .await
            .expect("single batch");
        let got = HearthStore::get_emotional_state(&store, "solo")
            .await
            .expect("get");
        assert!((got.joy - 0.9).abs() < f64::EPSILON);
    }

    /// Edge case: a batch update over an existing state replaces it.
    #[tokio::test]
    async fn batch_overwrites_existing() {
        let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        // Initial state.
        HearthStore::set_emotional_state(
            &store,
            "agent_a",
            EmotionalVector {
                joy: 0.1,
                ..EmotionalVector::neutral()
            },
        )
        .await
        .expect("initial set");

        // Batch replaces agent_a + adds agent_b.
        HearthStore::set_emotional_states_batch(
            &store,
            vec![
                (
                    "agent_a".to_string(),
                    EmotionalVector {
                        joy: 0.95,
                        ..EmotionalVector::neutral()
                    },
                ),
                (
                    "agent_b".to_string(),
                    EmotionalVector {
                        joy: 0.3,
                        ..EmotionalVector::neutral()
                    },
                ),
            ],
        )
        .await
        .expect("batch overwrite");

        let a = HearthStore::get_emotional_state(&store, "agent_a")
            .await
            .expect("get a");
        let b = HearthStore::get_emotional_state(&store, "agent_b")
            .await
            .expect("get b");
        assert!((a.joy - 0.95).abs() < f64::EPSILON, "agent_a overwritten");
        assert!((b.joy - 0.3).abs() < f64::EPSILON, "agent_b inserted");

        // No duplicates: agent_a appears exactly once.
        let agents = HearthStore::list_agents_with_emotion(&store)
            .await
            .expect("list");
        assert_eq!(agents.iter().filter(|x| *x == "agent_a").count(), 1);
        assert_eq!(agents.len(), 2);
    }

    /// Batch clamps values the same way a per-agent call does.
    #[tokio::test]
    async fn batch_clamps_like_per_agent() {
        let store = InMemoryHearthStore::new(LocalJsonStore::in_memory());
        let out_of_range = EmotionalVector {
            joy: 1.5,
            sadness: -0.3,
            ..EmotionalVector::neutral()
        };
        HearthStore::set_emotional_states_batch(&store, vec![("a".to_string(), out_of_range)])
            .await
            .expect("batch");
        let got = HearthStore::get_emotional_state(&store, "a")
            .await
            .expect("get");
        assert!((got.joy - 1.0).abs() < f64::EPSILON, "joy clamped to 1.0");
        assert!(
            (got.sadness - 0.0).abs() < f64::EPSILON,
            "sadness clamped to 0.0"
        );
    }

    #[tokio::test]
    async fn delegates_to_memory_store() {
        let mem = LocalJsonStore::in_memory();
        let store = InMemoryHearthStore::new(mem);

        assert!(familyclaw_memory::MemoryStore::is_empty(&store)
            .await
            .expect("is_empty"));

        let m = familyclaw_memory::Memory::builder("test").build();
        let _id = familyclaw_memory::MemoryStore::add(&store, m)
            .await
            .expect("add");

        assert!(!familyclaw_memory::MemoryStore::is_empty(&store)
            .await
            .expect("is_empty"));
    }
}

//! SurrealDB v3 backend for The Hearth.
//!
//! Implements [`HearthStore`] trait using SurrealDB (`surrealdb::Surreal<Any>`).
//! Supports in-memory dev and RocksDB production backends via the same client.
//!
//! Feature-gated behind `surreal` flag.

#[cfg(feature = "surreal")]
pub mod surreal {
    use crate::{
        emotional_state::EmotionalVector, narrative::EventType, HearthStore, NarrativeThread,
    };
    use familyclaw_core::Result;
    use std::sync::Arc;
    use surrealdb::{engine::any::Any, Surreal};
    use uuid::Uuid;

    /// SurrealDB-backed HearthStore implementation.
    #[derive(Clone)]
    pub struct SurrealHearthStore {
        db: Arc<Surreal<Any>>,
    }

    impl SurrealHearthStore {
        /// Connect to SurrealDB and initialize schema.
        ///
        /// # Arguments
        /// * `conn_str` - Connection string, e.g.:
        ///   - In-memory: `mem://`
        ///   - File (RocksDB): `rocksdb:///path/to/db`
        ///   - Remote: `ws://host:8000` or `wss://host:8000`
        ///
        /// # Errors
        /// Returns error if connection or schema initialization fails.
        pub async fn connect(conn_str: &str) -> Result<Self> {
            // SurrealDB v3: use engine::any::connect which handles all endpoint types
            let db = surrealdb::engine::any::connect(conn_str)
                .await
                .map_err(|e| {
                    familyclaw_core::FamilyClawError::Memory(format!(
                        "SurrealDB connect failed: {e}"
                    ))
                })?;

            // Use namespace/database
            db.use_ns("familyclaw")
                .use_db("hearth")
                .await
                .map_err(|e| {
                    familyclaw_core::FamilyClawError::Memory(format!("SurrealDB ns/db failed: {e}"))
                })?;

            // Initialize schema
            Self::init_schema(&db).await?;

            Ok(Self { db: Arc::new(db) })
        }

        /// Initialize the Hearth schema (tables, indexes).
        async fn init_schema(db: &Surreal<Any>) -> Result<()> {
            // Hardcoded schema to avoid include_str issues
            let schema_sql = r#"
DEFINE TABLE memory_event SCHEMAFULL;
DEFINE FIELD id ON memory_event TYPE string;
DEFINE FIELD content ON memory_event TYPE string;
DEFINE FIELD embedding ON memory_event TYPE array<float>;
DEFINE FIELD memory_type ON memory_event TYPE string;
DEFINE FIELD agent_id ON memory_event TYPE string;
DEFINE FIELD decay_class ON memory_event TYPE string;
DEFINE FIELD created_at ON memory_event TYPE datetime;
DEFINE FIELD participants ON memory_event TYPE array<string>;
DEFINE INDEX idx_embedding ON memory_event FIELDS embedding HNSW DIMENSION 1536;

DEFINE TABLE narrative_thread SCHEMAFULL;
DEFINE FIELD id ON narrative_thread TYPE string;
DEFINE FIELD title ON narrative_thread TYPE string;
DEFINE FIELD participants ON narrative_thread TYPE array<string>;
DEFINE FIELD created_at ON narrative_thread TYPE datetime;

DEFINE TABLE thread_event SCHEMAFULL;
DEFINE FIELD id ON thread_event TYPE string;
DEFINE FIELD thread_id ON thread_event TYPE string;
DEFINE FIELD event_type ON thread_event TYPE string;
DEFINE FIELD content ON thread_event TYPE string;
DEFINE FIELD agent_id ON thread_event TYPE string;
DEFINE FIELD linked_to ON thread_event TYPE array<string>;
DEFINE INDEX idx_thread ON thread_event FIELDS thread_id;

DEFINE TABLE emotional_state SCHEMAFULL;
DEFINE FIELD agent_id ON emotional_state TYPE string;
DEFINE FIELD joy ON emotional_state TYPE float;
DEFINE FIELD sadness ON emotional_state TYPE float;
DEFINE FIELD curiosity ON emotional_state TYPE float;
DEFINE FIELD anxiety ON emotional_state TYPE float;
DEFINE FIELD confidence ON emotional_state TYPE float;
DEFINE FIELD affection ON emotional_state TYPE float;
DEFINE FIELD updated_at ON emotional_state TYPE datetime;

DEFINE TABLE anchor SCHEMAFULL;
DEFINE FIELD agent_name ON anchor TYPE string;
DEFINE FIELD content_hash ON anchor TYPE string;
DEFINE FIELD protected ON anchor TYPE bool;
DEFINE FIELD decay_class ON anchor TYPE string;
"#;

            db.query(schema_sql).await.map_err(|e| {
                familyclaw_core::FamilyClawError::Memory(format!("Schema init failed: {e}"))
            })?;

            Ok(())
        }

        /// Get the underlying DB for advanced operations.
        pub fn db(&self) -> &Arc<Surreal<Any>> {
            &self.db
        }
    }

    impl HearthStore for SurrealHearthStore {
        // --- Narrative threads ---

        fn get_thread(
            &self,
            thread_id: Uuid,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<NarrativeThread>>> + Send + '_>,
        > {
            let db = Arc::clone(&self.db);
            Box::pin(async move {
                let rows: Vec<serde_json::Value> = db
                    .query("SELECT * FROM narrative_thread WHERE id = $thread_id")
                    .bind(("thread_id", thread_id.to_string()))
                    .await
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB query failed: {e}"
                        ))
                    })?
                    .take(0)
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB take failed: {e}"
                        ))
                    })?;

                if rows.is_empty() {
                    return Ok(None);
                }

                let row = &rows[0];
                let thread = NarrativeThread {
                    id: Uuid::parse_str(row.get("id").and_then(|v| v.as_str()).unwrap_or(""))
                        .unwrap_or_else(|_| Uuid::nil()),
                    title: row
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    participants: row
                        .get("participants")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    created_at: row
                        .get("created_at")
                        .and_then(|v| v.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                        .unwrap_or_else(chrono::Utc::now),
                    events: Vec::new(), // Events loaded separately
                };
                Ok(Some(thread))
            })
        }

        fn set_thread(
            &self,
            thread: NarrativeThread,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            let db = Arc::clone(&self.db);
            let id = thread.id.to_string();
            let title = thread.title;
            let participants = thread.participants;
            let created_at = thread.created_at.to_rfc3339();
            Box::pin(async move {
                db.query(
                    "UPSERT narrative_thread SET id = $id, title = $title, participants = $participants, created_at = $created_at"
                )
                .bind(("id", id))
                .bind(("title", title))
                .bind(("participants", participants))
                .bind(("created_at", created_at))
                .await
                .map_err(|e| familyclaw_core::FamilyClawError::Memory(format!("SurrealDB upsert failed: {e}")))?;
                Ok(())
            })
        }

        // --- Emotional state ---

        fn get_emotional_state(
            &self,
            agent_id: &str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<EmotionalVector>> + Send + '_>>
        {
            let db = Arc::clone(&self.db);
            let agent_id = agent_id.to_string();
            Box::pin(async move {
                let rows: Vec<serde_json::Value> = db
                    .query("SELECT * FROM emotional_state WHERE agent_id = $agent_id")
                    .bind(("agent_id", agent_id))
                    .await
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB query failed: {e}"
                        ))
                    })?
                    .take(0)
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB take failed: {e}"
                        ))
                    })?;

                if rows.is_empty() {
                    return Ok(EmotionalVector::neutral());
                }

                let row = &rows[0];
                Ok(EmotionalVector {
                    joy: row.get("joy").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    sadness: row.get("sadness").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    curiosity: row.get("curiosity").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    anxiety: row.get("anxiety").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    confidence: row
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0) as f32,
                    affection: row.get("affection").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                })
            })
        }

        fn set_emotional_state(
            &self,
            agent_id: &str,
            state: EmotionalVector,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            let db = Arc::clone(&self.db);
            let agent_id = agent_id.to_string();
            let updated_at = chrono::Utc::now().to_rfc3339();
            let joy = state.joy;
            let sadness = state.sadness;
            let curiosity = state.curiosity;
            let anxiety = state.anxiety;
            let confidence = state.confidence;
            let affection = state.affection;
            Box::pin(async move {
                db.query(
                    "UPSERT emotional_state SET agent_id = $agent_id, joy = $joy, sadness = $sadness, curiosity = $curiosity, anxiety = $anxiety, confidence = $confidence, affection = $affection, updated_at = $updated_at"
                )
                .bind(("agent_id", agent_id))
                .bind(("joy", joy))
                .bind(("sadness", sadness))
                .bind(("curiosity", curiosity))
                .bind(("anxiety", anxiety))
                .bind(("confidence", confidence))
                .bind(("affection", affection))
                .bind(("updated_at", updated_at))
                .await
                .map_err(|e| familyclaw_core::FamilyClawError::Memory(format!("SurrealDB upsert failed: {e}")))?;
                Ok(())
            })
        }

        fn list_agents_with_emotion(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>>> + Send + '_>>
        {
            let db = Arc::clone(&self.db);
            Box::pin(async move {
                let rows: Vec<serde_json::Value> = db
                    .query("SELECT agent_id FROM emotional_state")
                    .await
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB query failed: {e}"
                        ))
                    })?
                    .take(0)
                    .map_err(|e| {
                        familyclaw_core::FamilyClawError::Memory(format!(
                            "SurrealDB take failed: {e}"
                        ))
                    })?;

                Ok(rows
                    .iter()
                    .filter_map(|v| {
                        v.get("agent_id")
                            .and_then(|id| id.as_str())
                            .map(String::from)
                    })
                    .collect())
            })
        }
    }
}

#[cfg(all(test, feature = "surreal"))]
mod tests {
    use super::surreal::SurrealHearthStore;

    #[tokio::test]
    async fn surreal_hearth_store_connect_mem() {
        let store = SurrealHearthStore::connect("mem://").await;
        assert!(store.is_ok(), "Should connect to mem://");
    }
}

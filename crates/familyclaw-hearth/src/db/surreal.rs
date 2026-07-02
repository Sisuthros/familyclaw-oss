//! `SurrealDB` v3 backend for The Hearth.
//!
//! Implements [`crate::HearthStore`] trait using `SurrealDB` (`surrealdb::Surreal<Any>`).
//! Supports in-memory dev and `RocksDB` production backends via the same client.
//!
//! Feature-gated behind `surreal` flag.

/// `SurrealDB`-pohjaiset Hearth-toteutukset (feature-gated `surreal`).
#[cfg(feature = "surreal")]
#[allow(clippy::module_inception)]
pub mod surreal {
    use crate::{emotional_state::EmotionalVector, HearthStore, NarrativeThread};
    use familyclaw_core::Result;
    use familyclaw_memory::LocalJsonStore;
    use std::sync::Arc;
    use surrealdb::{engine::any::Any, Surreal};
    use uuid::Uuid;

    /// `SurrealDB`-backed [`HearthStore`] implementation.
    #[derive(Clone)]
    pub struct SurrealHearthStore {
        db: Arc<Surreal<Any>>,
        /// `MemoryStore`-supertraitin toteutus. `SurrealDB`-pohjainen
        /// `MemoryStore`-backend ei ole vielä valmis (skeeman `memory_event`
        /// -taulu on määritelty muttei kytketty), joten supertrait
        /// delegoidaan kevyeen muistinvaraiseen [`LocalJsonStore`]:hen.
        /// Tämä pitää `HearthStore: MemoryStore`-rajan tyydytettynä ilman
        /// `todo!()`/`unimplemented!()`-paniikkeja.
        memory: Arc<LocalJsonStore>,
    }

    impl SurrealHearthStore {
        /// Connect to `SurrealDB` and initialize schema.
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

            Ok(Self {
                db: Arc::new(db),
                memory: Arc::new(LocalJsonStore::in_memory()),
            })
        }

        /// Initialize the Hearth schema (tables, indexes).
        async fn init_schema(db: &Surreal<Any>) -> Result<()> {
            // Hardcoded schema to avoid include_str issues
            let schema_sql = r"
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
DEFINE FIELD thread_uid ON narrative_thread TYPE string;
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
";

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

    impl familyclaw_memory::MemoryStore for SurrealHearthStore {
        fn add(
            &self,
            memory: familyclaw_memory::Memory,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<familyclaw_core::MessageId>> + Send + '_>,
        > {
            self.memory.add(memory)
        }

        fn get(
            &self,
            id: familyclaw_core::MessageId,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<familyclaw_memory::Memory>>>
                    + Send
                    + '_,
            >,
        > {
            self.memory.get(id)
        }

        fn update(
            &self,
            memory: familyclaw_memory::Memory,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            self.memory.update(memory)
        }

        fn reinforce(
            &self,
            id: familyclaw_core::MessageId,
            at: familyclaw_core::Timestamp,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            self.memory.reinforce(id, at)
        }

        fn set_status(
            &self,
            id: familyclaw_core::MessageId,
            status: familyclaw_memory::MemoryStatus,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            self.memory.set_status(id, status)
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
            self.memory.all()
        }

        fn len(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<usize>> + Send + '_>>
        {
            self.memory.len()
        }

        fn is_empty(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + '_>>
        {
            self.memory.is_empty()
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
            self.memory.retrieve(ctx, at)
        }

        fn run_decay(
            &self,
            thresholds: familyclaw_memory::DecayThresholds,
            at: familyclaw_core::Timestamp,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<familyclaw_memory::DecayReport>>
                    + Send
                    + '_,
            >,
        > {
            self.memory.run_decay(thresholds, at)
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
                    .query("SELECT * FROM narrative_thread WHERE thread_uid = $thread_id")
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
                // `narrative_thread` ei tallenna erillistä `updated_at`-kenttää
                // (vrt. skeema), joten round-trippaa se `created_at`:sta kuten
                // `NarrativeThread::new` (luonnissa `updated_at == created_at`).
                let created_at = row
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map_or_else(chrono::Utc::now, |dt| dt.with_timezone(&chrono::Utc));
                let thread = NarrativeThread {
                    id: Uuid::parse_str(
                        row.get("thread_uid").and_then(|v| v.as_str()).unwrap_or(""),
                    )
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
                    created_at,
                    updated_at: created_at,
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
            // RFC3339-merkkijono castataan natiiviksi datetimeksi (`type::datetime`)
            // koska skeema määrittelee `created_at TYPE datetime` — pelkkä string
            // aiheutti hiljaisen tyyppirikkomuksen (sama bugi kuin emotional_statessa).
            let created_at = thread.created_at.to_rfc3339();
            Box::pin(async move {
                // Osoita rivi `type::record`:lla jotta UPSERT päivittää saman rivin
                // paikallaan — bare `UPSERT narrative_thread SET id=...` loi joka
                // kutsulla uuden satunnais-id-rivin eikä persistoinut oikein.
                // Tallenna UUID myös erilliseen `thread_uid`-string-kenttään, jotta
                // `get_thread` voi hakea sen tavallisena kenttänä (record-id palautuu
                // record-linkkinä, ei plain-stringinä — vrt. emotional_state.agent_id).
                let mut resp = db
                    .query(
                        "UPSERT type::record('narrative_thread', $id) SET thread_uid = $id, title = $title, participants = $participants, created_at = type::datetime($created_at)"
                    )
                    .bind(("id", id))
                    .bind(("title", title))
                    .bind(("participants", participants))
                    .bind(("created_at", created_at))
                    .await
                    .map_err(|e| familyclaw_core::FamilyClawError::Memory(format!("SurrealDB upsert failed: {e}")))?;
                // Statement-tason virheet eivät näy `.await`:ssa vaan `take_errors`:ssa.
                let errors = resp.take_errors();
                if !errors.is_empty() {
                    return Err(familyclaw_core::FamilyClawError::Memory(format!(
                        "SurrealDB set_thread statement error: {errors:?}"
                    )));
                }
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
                    joy: row
                        .get("joy")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    sadness: row
                        .get("sadness")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    curiosity: row
                        .get("curiosity")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    anxiety: row
                        .get("anxiety")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    confidence: row
                        .get("confidence")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
                    affection: row
                        .get("affection")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0),
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
            let state = state.clamped();
            Box::pin(async move {
                // Käytä eksplisiittistä record-id:tä (`emotional_state:<agent_id>`)
                // `type::record`:lla, jotta UPSERT päivittää saman rivin paikallaan
                // eikä luo uutta satunnais-id:tä joka kutsulla. `updated_at`
                // asetetaan `time::now()`:lla (natiivi datetime, SCHEMAFULL-kenttä).
                let mut resp = db
                    .query(
                        "UPSERT type::record('emotional_state', $agent_id) SET agent_id = $agent_id, joy = $joy, sadness = $sadness, curiosity = $curiosity, anxiety = $anxiety, confidence = $confidence, affection = $affection, updated_at = time::now()"
                    )
                    .bind(("agent_id", agent_id))
                    .bind(("joy", state.joy))
                    .bind(("sadness", state.sadness))
                    .bind(("curiosity", state.curiosity))
                    .bind(("anxiety", state.anxiety))
                    .bind(("confidence", state.confidence))
                    .bind(("affection", state.affection))
                    .await
                    .map_err(|e| familyclaw_core::FamilyClawError::Memory(format!("SurrealDB upsert failed: {e}")))?;
                // Pinta lauseiden virheet (SCHEMAFULL-rikkomukset yms. eivät
                // näy pelkässä `.await`:ssa vaan `take_errors`:ssa).
                let errors = resp.take_errors();
                if !errors.is_empty() {
                    return Err(familyclaw_core::FamilyClawError::Memory(format!(
                        "SurrealDB upsert statement error: {errors:?}"
                    )));
                }
                Ok(())
            })
        }

        fn set_emotional_states_batch(
            &self,
            states: Vec<(String, EmotionalVector)>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
            let db = Arc::clone(&self.db);
            Box::pin(async move {
                if states.is_empty() {
                    return Ok(());
                }

                // Rakenna YKSI query joka niputtaa kaikki agenttien UPSERTit
                // yhteen transaktioon → yksi tietokantakierros N:n sijaan.
                // Jokaiselle agentille indeksoidut bind-parametrit ($agent_0,
                // $joy_0, ...) — ei string-interpolaatiota arvoihin (injektiosuoja).
                let mut sql = String::from("BEGIN TRANSACTION;\n");
                for i in 0..states.len() {
                    use std::fmt::Write as _;
                    let _ = writeln!(
                        sql,
                        "UPSERT type::record('emotional_state', $agent_{i}) SET agent_id = $agent_{i}, joy = $joy_{i}, sadness = $sadness_{i}, curiosity = $curiosity_{i}, anxiety = $anxiety_{i}, confidence = $confidence_{i}, affection = $affection_{i}, updated_at = time::now();"
                    );
                }
                sql.push_str("COMMIT TRANSACTION;");

                let mut q = db.query(sql);
                for (i, (agent_id, state)) in states.into_iter().enumerate() {
                    let state = state.clamped();
                    q = q
                        .bind((format!("agent_{i}"), agent_id))
                        .bind((format!("joy_{i}"), state.joy))
                        .bind((format!("sadness_{i}"), state.sadness))
                        .bind((format!("curiosity_{i}"), state.curiosity))
                        .bind((format!("anxiety_{i}"), state.anxiety))
                        .bind((format!("confidence_{i}"), state.confidence))
                        .bind((format!("affection_{i}"), state.affection));
                }

                let mut resp = q.await.map_err(|e| {
                    familyclaw_core::FamilyClawError::Memory(format!(
                        "SurrealDB batch upsert failed: {e}"
                    ))
                })?;
                let errors = resp.take_errors();
                if !errors.is_empty() {
                    return Err(familyclaw_core::FamilyClawError::Memory(format!(
                        "SurrealDB batch upsert statement error: {errors:?}"
                    )));
                }
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
    use crate::emotional_state::EmotionalVector;
    use crate::HearthStore;
    use crate::NarrativeThread;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[tokio::test]
    async fn surreal_hearth_store_connect_mem() {
        let store = SurrealHearthStore::connect("mem://").await;
        assert!(store.is_ok(), "Should connect to mem://");
    }

    /// Yksittäinen set/get round-trippaa oikein (todistaa että record-id +
    /// `time::now()` -korjaus persistoi rivin — bare-UPSERT ei tehnyt).
    #[tokio::test]
    async fn surreal_emotional_state_single_roundtrip() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        let state = EmotionalVector {
            joy: 0.8,
            sadness: 0.1,
            curiosity: 0.6,
            anxiety: 0.2,
            confidence: 0.7,
            affection: 0.5,
        };
        store
            .set_emotional_state("agent_a", state)
            .await
            .expect("set");
        let got = store.get_emotional_state("agent_a").await.expect("get");
        assert_eq!(got, state, "single write must round-trip exactly");
    }

    /// Toistuva set samalle agentille päivittää PAIKALLAAN — ei duplikaatteja.
    #[tokio::test]
    async fn surreal_repeated_set_updates_in_place() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        for joy in [0.8_f64, 0.2, 0.55] {
            store
                .set_emotional_state(
                    "agent_a",
                    EmotionalVector {
                        joy,
                        ..EmotionalVector::neutral()
                    },
                )
                .await
                .expect("set");
        }
        let got = store.get_emotional_state("agent_a").await.expect("get");
        assert!(approx(got.joy, 0.55), "last write wins");
        // Tasan yksi agentti rekisterissä (ei satunnais-id-duplikaatteja).
        let agents = store.list_agents_with_emotion().await.expect("list");
        assert_eq!(
            agents.iter().filter(|a| *a == "agent_a").count(),
            1,
            "no duplicate rows"
        );
    }

    /// Batch tuottaa saman lopputilan kuin per-agentti-kutsut samaan dataan.
    #[tokio::test]
    async fn surreal_batch_equals_per_agent() {
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
        ];

        let per_agent = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        for (agent, state) in &states {
            per_agent
                .set_emotional_state(agent, *state)
                .await
                .expect("set per-agent");
        }

        let batch = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        batch
            .set_emotional_states_batch(states.clone())
            .await
            .expect("batch");

        for (agent, _) in &states {
            let a = per_agent
                .get_emotional_state(agent)
                .await
                .expect("get per-agent");
            let b = batch.get_emotional_state(agent).await.expect("get batch");
            assert_eq!(a, b, "batch vs per-agent mismatch for {agent}");
        }

        let mut la = per_agent.list_agents_with_emotion().await.expect("list a");
        let mut lb = batch.list_agents_with_emotion().await.expect("list b");
        la.sort();
        lb.sort();
        assert_eq!(la, lb, "agent sets must match");
    }

    /// Reunatapaus: 0 agenttia — no-op, ei virhettä, ei rivejä.
    #[tokio::test]
    async fn surreal_batch_empty_is_noop() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        store
            .set_emotional_states_batch(vec![])
            .await
            .expect("empty batch");
        let agents = store.list_agents_with_emotion().await.expect("list");
        assert!(agents.is_empty());
    }

    /// Reunatapaus: 1 agentti batchissa.
    #[tokio::test]
    async fn surreal_batch_single_agent() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        let state = EmotionalVector {
            joy: 0.9,
            ..EmotionalVector::neutral()
        };
        store
            .set_emotional_states_batch(vec![("solo".to_string(), state)])
            .await
            .expect("single batch");
        let got = store.get_emotional_state("solo").await.expect("get");
        assert_eq!(got, state);
    }

    /// Reunatapaus: batch olemassa olevan tilan päälle korvaa paikallaan.
    #[tokio::test]
    async fn surreal_batch_overwrites_existing() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        store
            .set_emotional_state(
                "agent_a",
                EmotionalVector {
                    joy: 0.1,
                    ..EmotionalVector::neutral()
                },
            )
            .await
            .expect("initial");

        store
            .set_emotional_states_batch(vec![
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
            ])
            .await
            .expect("batch overwrite");

        let a = store.get_emotional_state("agent_a").await.expect("get a");
        let b = store.get_emotional_state("agent_b").await.expect("get b");
        assert!(approx(a.joy, 0.95), "agent_a overwritten in place");
        assert!(approx(b.joy, 0.3), "agent_b inserted");

        let agents = store.list_agents_with_emotion().await.expect("list");
        assert_eq!(
            agents.iter().filter(|x| *x == "agent_a").count(),
            1,
            "no duplicate agent_a"
        );
        assert_eq!(agents.len(), 2);
    }

    /// Batch clampaa arvot samoin kuin per-agentti-polku.
    #[tokio::test]
    async fn surreal_batch_clamps() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        store
            .set_emotional_states_batch(vec![(
                "a".to_string(),
                EmotionalVector {
                    joy: 1.5,
                    sadness: -0.2,
                    ..EmotionalVector::neutral()
                },
            )])
            .await
            .expect("batch");
        let got = store.get_emotional_state("a").await.expect("get");
        assert!(approx(got.joy, 1.0), "joy clamped");
        assert!(approx(got.sadness, 0.0), "sadness clamped");
    }

    /// Todistaa `set_thread`-korjauksen: thread persistoituu ja round-trippaa
    /// oikein (aiempi bare-UPSERT + RFC3339-string-datetime esti persistoinnin).
    #[tokio::test]
    async fn surreal_thread_roundtrip() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        let thread = NarrativeThread::new(
            "shared-project",
            vec!["agent_alpha".into(), "agent_beta".into()],
        );
        let id = thread.id;
        store.set_thread(thread.clone()).await.expect("set_thread");

        let got = store.get_thread(id).await.expect("get_thread");
        let got = got.expect("thread persisted (was silently dropped before fix)");
        assert_eq!(got.id, id);
        assert_eq!(got.title, "shared-project");
        assert_eq!(
            got.participants,
            vec!["agent_alpha".to_string(), "agent_beta".to_string()]
        );
    }

    /// Toistuva `set_thread` samalla id:llä päivittää saman rivin paikallaan,
    /// ei luo duplikaatteja (bare-UPSERT loi joka kutsulla uuden satunnais-id-rivin).
    #[tokio::test]
    async fn surreal_thread_set_updates_in_place() {
        let store = SurrealHearthStore::connect("mem://")
            .await
            .expect("connect");
        let mut thread = NarrativeThread::new("v1", vec!["agent_alpha".into()]);
        let id = thread.id;
        store.set_thread(thread.clone()).await.expect("set v1");

        thread.title = "v2".to_string();
        store.set_thread(thread).await.expect("set v2");

        let got = store
            .get_thread(id)
            .await
            .expect("get")
            .expect("still one thread");
        assert_eq!(got.title, "v2", "update replaced in place");
    }
}

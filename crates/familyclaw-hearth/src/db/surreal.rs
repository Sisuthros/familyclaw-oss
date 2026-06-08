//! SurrealDB v3 -pohjainen [`HearthStore`]-toteutus (`surreal`-feature).
//!
//! [`SurrealHearthStore`] käärii `Surreal<Any>`-yhteyden (in-mem dev tai
//! `RocksDB` prod) ja toteuttaa sekä [`familyclaw_memory::MemoryStore`]:n että
//! [`HearthStore`]:n. Skeema ([`crate::db::schema::HEARTH_SCHEMA`]) sovelletaan
//! yhteyden alustuksessa.
//!
//! ## Tärkeää: `serde_json::Value` välikätenä
//! SurrealDB v3 palauttaa omat tietueensa; tässä toteutuksessa **emme**
//! deserialisoi suoraan domain-structeihin vaan kuljetamme datan
//! [`serde_json::Value`]:n kautta ja `serde_json::from_value`/`to_value`
//! -muunnoksilla. Tämä eristää meidät SurrealDB:n tietuetyypeistä (esim. v2:n
//! `Thing`) ja pitää backendin vaihdettavana.
//!
//! ## Stub-status
//! Tämä on tarkoituksellisesti **stub**: se kääntyy `surreal`-featuren takana,
//! mutta sitä ei ajeta yksikkötesteissä (ei vaadi käynnissä olevaa
//! SurrealDB-palvelinta). Oletustoteutus on edelleen
//! [`crate::db::InMemoryHearthStore`].

use familyclaw_core::{FamilyClawError, MessageId, Result, Timestamp};
use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use uuid::Uuid;

use crate::db::schema::HEARTH_SCHEMA;
use crate::db::HearthStore;
use crate::emotional_state::EmotionalVector;
use crate::narrative::NarrativeThread;

use familyclaw_memory::{
    DecayReport, DecayThresholds, Memory, MemoryStatus, MemoryStore, RetrievalContext,
    RetrievalResult,
};

/// Tyyppieristetty future dyn-yhteensopivuutta varten (vrt. [`MemoryStore`]).
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Kääntää SurrealDB-virheen [`FamilyClawError::Memory`]:ksi.
fn map_db_err(e: impl std::fmt::Display) -> FamilyClawError {
    FamilyClawError::Memory(format!("surreal: {e}"))
}

/// SurrealDB v3 -pohjainen Hearth-tallennus.
///
/// Käärii `Surreal<Any>`-yhteyden. Luo [`SurrealHearthStore::connect`]:llä.
pub struct SurrealHearthStore {
    /// Tietokantayhteys (in-mem dev tai `RocksDB` prod).
    db: Surreal<Any>,
}

impl SurrealHearthStore {
    /// Avaa yhteyden annettuun endpointiin ja soveltaa skeeman.
    ///
    /// `endpoint` on esim. `"mem://"` (dev) tai `"rocksdb://path"` (prod).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos yhteys, namespace/db-valinta tai skeeman
    /// soveltaminen epäonnistuu.
    pub async fn connect(endpoint: &str, ns: &str, db: &str) -> Result<Self> {
        // Varoita salaamattomasta etäyhteydestä: paikalliset enginet
        // (`mem://`, `rocksdb://`, `surrealkv://`, `file://`) ovat turvallisia,
        // mutta `ws://`/`http://` lähettää liikenteen selväkielisenä. Käytä
        // tuotannossa `wss://`/`https://`.
        if endpoint.starts_with("ws://") || endpoint.starts_with("http://") {
            eprintln!(
                "WARN: SurrealHearthStore yhdistää salaamattomaan endpointiin \
                 ({endpoint}); käytä tuotannossa wss://- tai https://-osoitetta."
            );
        }
        let conn = surrealdb::engine::any::connect(endpoint)
            .await
            .map_err(map_db_err)?;
        conn.use_ns(ns).use_db(db).await.map_err(map_db_err)?;
        conn.query(HEARTH_SCHEMA).await.map_err(map_db_err)?;
        Ok(Self { db: conn })
    }

    /// Käärii valmiin yhteyden (skeemaa ei sovelleta uudelleen).
    #[must_use]
    pub fn from_connection(db: Surreal<Any>) -> Self {
        Self { db }
    }
}

impl MemoryStore for SurrealHearthStore {
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>> {
        Box::pin(async move {
            let id = memory.id;
            // serde_json::Value välikätenä — EI suoraa struct-bindausta.
            let data = serde_json::to_value(&memory)?;
            self.db
                .query("CREATE memory_event CONTENT $data")
                .bind(("data", data))
                .await
                .map_err(map_db_err)?;
            Ok(id)
        })
    }

    fn get(&self, id: MessageId) -> BoxFuture<'_, Result<Option<Memory>>> {
        Box::pin(async move {
            let mut res = self
                .db
                .query("SELECT * FROM memory_event WHERE id = $id")
                .bind(("id", id.to_string()))
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            match rows.into_iter().next() {
                Some(v) => Ok(Some(serde_json::from_value(v)?)),
                None => Ok(None),
            }
        })
    }

    fn update(&self, memory: Memory) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let id = memory.id.to_string();
            let data = serde_json::to_value(&memory)?;
            self.db
                .query("UPDATE memory_event CONTENT $data WHERE id = $id")
                .bind(("data", data))
                .bind(("id", id))
                .await
                .map_err(map_db_err)?;
            Ok(())
        })
    }

    fn reinforce(&self, id: MessageId, at: Timestamp) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            // Stub: lue–muokkaa–kirjoita domain-tyypin kautta.
            let Some(mut memory) = self.get(id).await? else {
                return Err(FamilyClawError::NotFound(format!("memory {id}")));
            };
            memory.reinforce(at);
            self.update(memory).await
        })
    }

    fn set_status(&self, id: MessageId, status: MemoryStatus) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let Some(mut memory) = self.get(id).await? else {
                return Err(FamilyClawError::NotFound(format!("memory {id}")));
            };
            memory.status = status;
            self.update(memory).await
        })
    }

    fn all(&self) -> BoxFuture<'_, Result<Vec<Memory>>> {
        Box::pin(async move {
            let mut res = self
                .db
                .query("SELECT * FROM memory_event")
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            rows.into_iter()
                .map(|v| serde_json::from_value(v).map_err(Into::into))
                .collect()
        })
    }

    fn len(&self) -> BoxFuture<'_, Result<usize>> {
        Box::pin(async move {
            // Laske tietokannassa — älä lataa kaikkia rivejä muistiin.
            let mut res = self
                .db
                .query("SELECT count() FROM memory_event GROUP ALL")
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            Ok(rows
                .first()
                .and_then(|v| v.get("count").and_then(serde_json::Value::as_u64))
                .unwrap_or(0) as usize)
        })
    }

    fn is_empty(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async move { Ok(self.len().await? == 0) })
    }

    fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<Vec<RetrievalResult>>> {
        // Stub: hae kaikki muistot tietokannasta ja delegoi jaettuun
        // pisteytys-/lajittelulogiikkaan (sama kuin LocalJsonStore).
        //
        // TODO(perf): tämä lataa KAIKKI muistot muistiin ja pisteyttää ne
        // sovelluskerroksessa. Tuotannossa tämä pitäisi työntää tietokantaan:
        // `SELECT * FROM memory_event WHERE query_string CONTAINS $q` +
        // emotion-/recency-pisteytys SurrealQL:ssä, jotta isot korpukset eivät
        // vuoda muistiin. Rajoitus: O(n) muistinkäyttö muistojen määrässä.
        let ctx = ctx.clone();
        Box::pin(async move {
            let memories = self.all().await?;
            Ok(familyclaw_memory::retrieve(&memories, &ctx, at))
        })
    }

    fn run_decay(
        &self,
        thresholds: DecayThresholds,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<DecayReport>> {
        Box::pin(async move {
            // Stub: lataa, sovella vaimennusta domain-tyypin kautta,
            // kirjoita muuttuneet takaisin. Sama logiikka kuin
            // LocalJsonStore::run_decay (status-elinkaari, suojattu ydin).
            let mut report = DecayReport::default();
            for mut memory in self.all().await? {
                report.scanned += 1;
                if memory.decay_policy.is_protected() {
                    continue;
                }
                let retention = memory.retention(at);
                let prev = memory.status;
                match memory.status {
                    MemoryStatus::Active => {
                        if retention < thresholds.archive_below {
                            memory.status = MemoryStatus::Archived;
                            report.archived += 1;
                        }
                    }
                    MemoryStatus::Archived => {
                        if retention < thresholds.tombstone_below {
                            memory.status = MemoryStatus::Tombstoned;
                            report.tombstoned += 1;
                        }
                    }
                    MemoryStatus::Tombstoned => {}
                }
                if memory.status != prev {
                    self.update(memory).await?;
                }
            }
            Ok(report)
        })
    }
}

impl HearthStore for SurrealHearthStore {
    // `create_thread` ja `add_thread_event` käyttävät trait-default-toteutuksia
    // jotka rakentuvat `get_thread`:n ja `set_thread`:n päälle — ei duplikaatiota.

    fn set_thread(&self, thread: NarrativeThread) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let id = thread.id.to_string();
            let data = serde_json::to_value(&thread)?;
            // Upsert: korvaa olemassa oleva tai luo uusi.
            self.db
                .query(
                    "UPDATE narrative_thread CONTENT $data WHERE id = $id \
                     ELSE CREATE narrative_thread CONTENT $data",
                )
                .bind(("data", data))
                .bind(("id", id))
                .await
                .map_err(map_db_err)?;
            Ok(())
        })
    }

    fn get_thread(&self, thread_id: Uuid) -> BoxFuture<'_, Result<Option<NarrativeThread>>> {
        Box::pin(async move {
            let mut res = self
                .db
                .query("SELECT * FROM narrative_thread WHERE id = $id")
                .bind(("id", thread_id.to_string()))
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            match rows.into_iter().next() {
                Some(v) => Ok(Some(serde_json::from_value(v)?)),
                None => Ok(None),
            }
        })
    }

    fn get_emotional_state(&self, agent_id: &str) -> BoxFuture<'_, Result<EmotionalVector>> {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let mut res = self
                .db
                .query("SELECT * FROM emotional_state WHERE agent_id = $aid")
                .bind(("aid", agent_id))
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            match rows.into_iter().next() {
                Some(v) => Ok(serde_json::from_value(v)?),
                None => Ok(EmotionalVector::neutral()),
            }
        })
    }

    fn set_emotional_state(
        &self,
        agent_id: &str,
        state: EmotionalVector,
    ) -> BoxFuture<'_, Result<()>> {
        let agent_id = agent_id.to_string();
        Box::pin(async move {
            let data = serde_json::to_value(state.clamped())?;
            self.db
                .query(
                    "UPDATE emotional_state CONTENT $data WHERE agent_id = $aid \
                     ELSE CREATE emotional_state CONTENT $data",
                )
                .bind(("data", data))
                .bind(("aid", agent_id))
                .await
                .map_err(map_db_err)?;
            Ok(())
        })
    }

    fn list_agents_with_emotion(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async move {
            let mut res = self
                .db
                .query("SELECT agent_id FROM emotional_state")
                .await
                .map_err(map_db_err)?;
            let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
            Ok(rows
                .into_iter()
                .filter_map(|v| {
                    v.get("agent_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect())
        })
    }
}

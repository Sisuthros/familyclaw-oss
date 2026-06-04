//! Muistin tallennus: [`MemoryStore`]-trait ja [`LocalJsonStore`].
//!
//! [`MemoryStore`] on Eternal Threadin tallennusabstraktio: lisää, hae,
//! päivitä elinkaaritila, suorita haku ja aja vaimennus-läpikäynti.
//! Oletustoteutus [`LocalJsonStore`] pitää muistot muistissa ja persistoi
//! ne JSON-tiedostoon atomisella kirjoituksella (tmp + rename).
//!
//! ## Tuleva: `Surreal<Any>` feature-flagin takana
//! Design (§2.3, §5) valitsee tuotantotallennukseksi SurrealDB:n
//! (`Surreal<Any>`: in-mem dev / `RocksDB` prod). Se lisätään myöhemmin
//! omana toteutuksenaan `surreal`-feature-flagin taakse — sama
//! [`MemoryStore`]-rajapinta, eri backend. [`LocalJsonStore`] säilyy
//! kevyenä, riippuvuusvapaana oletuksena (KERROS A toimii ilman natiivia
//! tietokantaa).
//!
//! Trait käyttää natiivia `async fn`-syntaksia (Rust ≥ 1.75). `Send`-rajat
//! varmistetaan testeissä, jotta toteutukset toimivat
//! monisäikeisessä tokio-ajossa.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use familyclaw_core::{FamilyClawError, MessageId, Result, Timestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::memory::{Memory, MemoryStatus};
use crate::retrieval::{retrieve, RetrievalContext, RetrievalResult};

/// Yhteenveto vaimennus-läpikäynnistä ([`MemoryStore::run_decay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DecayReport {
    /// Aktiivisesta arkistoon siirrettyjen muistojen määrä.
    pub archived: usize,
    /// Arkistosta haudattujen (tombstoned) muistojen määrä.
    pub tombstoned: usize,
    /// Läpikäytyjen muistojen kokonaismäärä.
    pub scanned: usize,
}

/// Kynnysarvot vaimennus-läpikäynnille.
///
/// Retention putoaa ajan myötä; kun se alittaa kynnyksen, muisto siirtyy
/// elinkaaren seuraavaan vaiheeseen. Suojattua ydintä
/// ([`crate::DecayPolicy::ProtectedCore`]) ei koskaan siirretä.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecayThresholds {
    /// Retention alapuolella aktiivinen muisto arkistoidaan.
    pub archive_below: f32,
    /// Retention alapuolella arkistoitu muisto haudataan.
    pub tombstone_below: f32,
}

impl DecayThresholds {
    /// Rakentaa kynnykset puristaen molemmat välille `0.0..=1.0` ja
    /// varmistaen `tombstone_below <= archive_below`.
    #[must_use]
    pub fn new(archive_below: f32, tombstone_below: f32) -> Self {
        let archive = clamp_unit(archive_below, 0.4);
        let tombstone = clamp_unit(tombstone_below, 0.1).min(archive);
        Self {
            archive_below: archive,
            tombstone_below: tombstone,
        }
    }
}

impl Default for DecayThresholds {
    /// Oletus: arkistoi alle `0.4`, hautaa alle `0.1`.
    fn default() -> Self {
        Self::new(0.4, 0.1)
    }
}

/// Puristaa arvon välille `0.0..=1.0`; kelvoton → `fallback`.
fn clamp_unit(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

/// Muistin tallennusabstraktio.
///
/// Toteutukset vastaavat persistoinnista ja samanaikaisuudesta. Kaikki
/// metodit ovat asynkronisia, jotta tietokantapohjaiset backendit
/// (`Surreal<Any>`) mahtuvat samaan rajapintaan.
pub trait MemoryStore {
    /// Lisää muiston tallennukseen ja palauttaa sen tunnisteen.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos tallennus epäonnistuu.
    fn add(&self, memory: Memory) -> impl std::future::Future<Output = Result<MessageId>> + Send;

    /// Hakee muiston tunnisteella, tai `None` jos ei löydy.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos haku epäonnistuu.
    fn get(
        &self,
        id: MessageId,
    ) -> impl std::future::Future<Output = Result<Option<Memory>>> + Send;

    /// Korvaa olemassa olevan muiston (sama `id`).
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos tunnistetta ei ole, tai
    /// [`FamilyClawError::Memory`] tallennusvirheestä.
    fn update(&self, memory: Memory) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Vahvistaa muiston (nostaa retention + tärkeyttä) hetkeen `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos tunnistetta ei ole.
    fn reinforce(
        &self,
        id: MessageId,
        at: Timestamp,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Asettaa muiston elinkaaritilan suoraan.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos tunnistetta ei ole.
    fn set_status(
        &self,
        id: MessageId,
        status: MemoryStatus,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Palauttaa kaikki muistot (myös arkistoidut/haudatut).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos luku epäonnistuu.
    fn all(&self) -> impl std::future::Future<Output = Result<Vec<Memory>>> + Send;

    /// Muistojen kokonaismäärä.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos luku epäonnistuu.
    fn len(&self) -> impl std::future::Future<Output = Result<usize>> + Send;

    /// Onko tallennus tyhjä.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos luku epäonnistuu.
    fn is_empty(&self) -> impl std::future::Future<Output = Result<bool>> + Send
    where
        Self: Sync,
    {
        async { Ok(self.len().await? == 0) }
    }

    /// Suorittaa haun annetulla kontekstilla ajanhetkellä `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos luku epäonnistuu.
    fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> impl std::future::Future<Output = Result<Vec<RetrievalResult>>> + Send;

    /// Ajaa vaimennus-läpikäynnin hetkeen `at`: siirtää alle kynnyksen
    /// pudonneet muistot arkistoon ja haudattaviksi. Suojattua ydintä ei
    /// koskaan siirretä.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos tallennus epäonnistuu.
    fn run_decay(
        &self,
        thresholds: DecayThresholds,
        at: Timestamp,
    ) -> impl std::future::Future<Output = Result<DecayReport>> + Send;
}

/// JSON-tiedostoon persistoiva muistitallennus.
///
/// Pitää muistot muistissa [`RwLock`]-suojattuna ja kirjoittaa ne levylle
/// atomisesti (tmp-tiedosto + `rename`) jokaisen mutaation jälkeen. Tämä on
/// KERROS A:n riippuvuusvapaa oletustoteutus — ei vaadi natiivia
/// tietokantaa eikä C/C++-toolchainia (vrt. design §5: rajoitetulla
/// kohdekoneella ei välttämättä ole RocksDB-toolchainia).
#[derive(Debug)]
pub struct LocalJsonStore {
    /// Tiedoston polku, tai `None` jos puhtaasti muistinvarainen.
    path: Option<PathBuf>,
    /// Muistit tunnisteittain.
    memories: RwLock<HashMap<MessageId, Memory>>,
}

/// JSON-tiedoston levymuoto (versioitu eteenpäinyhteensopivuutta varten).
#[derive(Debug, Serialize, Deserialize)]
struct DiskFormat {
    /// Tiedostoformaatin versio.
    version: u32,
    /// Tallennetut muistot.
    memories: Vec<Memory>,
}

impl DiskFormat {
    const CURRENT_VERSION: u32 = 1;
}

impl LocalJsonStore {
    /// Luo puhtaasti muistinvaraisen tallennuksen (ei levypersistointia).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            memories: RwLock::new(HashMap::new()),
        }
    }

    /// Avaa (tai luo) JSON-tallennuksen annetusta polusta.
    ///
    /// Jos tiedosto on olemassa, sen muistot ladataan. Jos ei, tallennus
    /// alkaa tyhjänä ja tiedosto luodaan ensimmäisellä kirjoituksella.
    ///
    /// # Errors
    /// [`FamilyClawError::Io`] jos olemassa olevaa tiedostoa ei voi lukea,
    /// tai [`FamilyClawError::Serde`] jos sen sisältö on kelvotonta JSON:ia.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let memories = if path.exists() {
            let contents = tokio::fs::read_to_string(&path).await?;
            let disk: DiskFormat = serde_json::from_str(&contents)?;
            disk.memories.into_iter().map(|m| (m.id, m)).collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path: Some(path),
            memories: RwLock::new(memories),
        })
    }

    /// Persistoi nykyisen tilan levylle atomisesti, jos polku on asetettu.
    ///
    /// Kutsuja pitää lukon; tämä ottaa snapshotin annetusta kartasta.
    async fn persist(&self, map: &HashMap<MessageId, Memory>) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut memories: Vec<Memory> = map.values().cloned().collect();
        // Vakaa järjestys diffattavaa, deterministista tiedostoa varten.
        memories.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let disk = DiskFormat {
            version: DiskFormat::CURRENT_VERSION,
            memories,
        };
        let json = serde_json::to_string_pretty(&disk)?;

        // Atominen kirjoitus: kirjoita tmp-tiedostoon, sitten rename.
        let tmp = tmp_path(path);
        tokio::fs::write(&tmp, json.as_bytes()).await?;
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    /// Palauttaa tallennustiedoston polun (tai `None` jos muistinvarainen).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Johtaa väliaikaistiedoston polun (`<path>.tmp`).
fn tmp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

impl MemoryStore for LocalJsonStore {
    async fn add(&self, memory: Memory) -> Result<MessageId> {
        let id = memory.id;
        // Idempotentti kirjaus: jos samalla turn_key:llä on jo muisto,
        // ohita (dual-write-suoja: durable.step voi onnistua vaikka
        // memory_store.add ei ehdi ennen kaatumista).
        if let Some(ref key) = memory.turn_key {
            let guard = self.memories.read().await;
            let exists = guard.values().any(|m| m.turn_key.as_ref() == Some(key));
            if exists {
                return Ok(id);
            }
        }
        let mut guard = self.memories.write().await;
        guard.insert(id, memory);
        self.persist(&guard).await?;
        Ok(id)
    }

    async fn get(&self, id: MessageId) -> Result<Option<Memory>> {
        let guard = self.memories.read().await;
        Ok(guard.get(&id).cloned())
    }

    async fn update(&self, memory: Memory) -> Result<()> {
        let mut guard = self.memories.write().await;
        if !guard.contains_key(&memory.id) {
            return Err(FamilyClawError::not_found(format!(
                "memory {} not found",
                memory.id
            )));
        }
        guard.insert(memory.id, memory);
        self.persist(&guard).await?;
        Ok(())
    }

    async fn reinforce(&self, id: MessageId, at: Timestamp) -> Result<()> {
        let mut guard = self.memories.write().await;
        let memory = guard
            .get_mut(&id)
            .ok_or_else(|| FamilyClawError::not_found(format!("memory {id} not found")))?;
        memory.reinforce(at);
        self.persist(&guard).await?;
        Ok(())
    }

    async fn set_status(&self, id: MessageId, status: MemoryStatus) -> Result<()> {
        let mut guard = self.memories.write().await;
        let memory = guard
            .get_mut(&id)
            .ok_or_else(|| FamilyClawError::not_found(format!("memory {id} not found")))?;
        memory.status = status;
        self.persist(&guard).await?;
        Ok(())
    }

    async fn all(&self) -> Result<Vec<Memory>> {
        let guard = self.memories.read().await;
        Ok(guard.values().cloned().collect())
    }

    async fn len(&self) -> Result<usize> {
        let guard = self.memories.read().await;
        Ok(guard.len())
    }

    async fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> Result<Vec<RetrievalResult>> {
        let guard = self.memories.read().await;
        Ok(retrieve(guard.values(), ctx, at))
    }

    async fn run_decay(&self, thresholds: DecayThresholds, at: Timestamp) -> Result<DecayReport> {
        let mut guard = self.memories.write().await;
        let mut report = DecayReport::default();
        for memory in guard.values_mut() {
            report.scanned += 1;
            // Suojattu ydin ohitetaan kokonaan.
            if memory.decay_policy.is_protected() {
                continue;
            }
            let retention = memory.retention(at);
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
        }
        self.persist(&guard).await?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::decay::DecayPolicy;
    use crate::importance::ImportanceFactors;
    use chrono::Duration;
    use familyclaw_core::time;
    use familyclaw_emotion::Dimension;

    fn mem(content: &str) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .build()
    }

    #[test]
    fn store_is_send() {
        // Varmistaa että LocalJsonStore voi liikkua säikeiden välillä.
        fn assert_send<T: Send>() {}
        assert_send::<LocalJsonStore>();
    }

    #[tokio::test]
    async fn add_get_and_len() {
        let store = LocalJsonStore::in_memory();
        assert!(store.is_empty().await.expect("empty check"));
        let m = mem("first");
        let id = store.add(m.clone()).await.expect("add");
        assert_eq!(store.len().await.expect("len"), 1);
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.content, "first");
        assert!(store
            .get(MessageId::new())
            .await
            .expect("get missing")
            .is_none());
    }

    #[tokio::test]
    async fn update_existing_and_missing() {
        let store = LocalJsonStore::in_memory();
        let mut m = mem("original");
        let id = store.add(m.clone()).await.expect("add");
        m.content = "edited".into();
        store.update(m.clone()).await.expect("update");
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.content, "edited");

        // Tuntematon id → NotFound.
        let ghost = mem("ghost");
        let err = store.update(ghost).await.expect_err("update missing fails");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn reinforce_updates_memory() {
        let store = LocalJsonStore::in_memory();
        let m = mem("reinforce me");
        let id = store.add(m).await.expect("add");
        let before = store.get(id).await.expect("g").expect("p").importance;
        store.reinforce(id, time::now()).await.expect("reinforce");
        let after = store.get(id).await.expect("g").expect("p");
        assert_eq!(after.reinforcement_count, 1);
        assert!(after.importance > before);

        let err = store
            .reinforce(MessageId::new(), time::now())
            .await
            .expect_err("missing");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn set_status_transitions() {
        let store = LocalJsonStore::in_memory();
        let id = store.add(mem("x")).await.expect("add");
        store
            .set_status(id, MemoryStatus::Archived)
            .await
            .expect("set status");
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );
    }

    #[tokio::test]
    async fn retrieve_through_store() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("rust memory engine")).await.expect("a1");
        store.add(mem("python web framework")).await.expect("a2");
        let ctx = RetrievalContext::new("rust memory");
        let results = store.retrieve(&ctx, time::now()).await.expect("retrieve");
        assert!(!results.is_empty());
        assert!(results[0].memory.content.contains("rust"));
    }

    #[tokio::test]
    async fn run_decay_archives_then_tombstones() {
        let store = LocalJsonStore::in_memory();
        let created = time::now();
        // Nopeasti vaimeneva, matala tärkeys.
        let m = Memory::builder("ephemeral")
            .factors(ImportanceFactors::new(0.05, 0.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::Fast)
            .created_at(created)
            .build();
        let id = store.add(m).await.expect("add");

        // Pitkän ajan kuluttua retention on hyvin matala → arkistoidaan.
        let later = created + Duration::days(30);
        let r1 = store
            .run_decay(DecayThresholds::default(), later)
            .await
            .expect("decay 1");
        assert_eq!(r1.scanned, 1);
        assert_eq!(r1.archived, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );

        // Toinen läpikäynti vielä myöhemmin → haudataan.
        let much_later = created + Duration::days(120);
        let r2 = store
            .run_decay(DecayThresholds::default(), much_later)
            .await
            .expect("decay 2");
        assert_eq!(r2.tombstoned, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned
        );
    }

    #[tokio::test]
    async fn run_decay_never_touches_protected_core() {
        let store = LocalJsonStore::in_memory();
        let created = time::now();
        let m = Memory::builder("identity anchor")
            .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .created_at(created)
            .build();
        let id = store.add(m).await.expect("add");
        let far = created + Duration::days(100_000);
        let report = store
            .run_decay(DecayThresholds::default(), far)
            .await
            .expect("decay");
        assert_eq!(report.archived, 0);
        assert_eq!(report.tombstoned, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn json_persistence_roundtrip() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-store-{}.json",
            uuid::Uuid::new_v4()
        ));

        let id = {
            let store = LocalJsonStore::open(&path).await.expect("open new");
            assert!(store.path().is_some());
            let m = Memory::builder("persisted")
                .emotions([Dimension::Gratitude])
                .factors(ImportanceFactors::new(0.7, 0.3, 0.0, 0.0))
                .source("test")
                .build();
            store.add(m).await.expect("add")
        };

        // Avaa uudestaan → data säilyi.
        let reopened = LocalJsonStore::open(&path).await.expect("reopen");
        assert_eq!(reopened.len().await.expect("len"), 1);
        let got = reopened.get(id).await.expect("g").expect("p");
        assert_eq!(got.content, "persisted");
        assert_eq!(got.emotions, vec![Dimension::Gratitude]);
        assert_eq!(got.source, "test");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn open_missing_file_starts_empty() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-absent-{}.json",
            uuid::Uuid::new_v4()
        ));
        // Varmista ettei ole.
        let _ = std::fs::remove_file(&path);
        let store = LocalJsonStore::open(&path).await.expect("open");
        assert!(store.is_empty().await.expect("empty"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn open_corrupt_file_errors() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-memory-corrupt-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "{ not valid json").expect("write garbage");
        let err = LocalJsonStore::open(&path)
            .await
            .expect_err("corrupt errors");
        assert!(matches!(err, FamilyClawError::Serde(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decay_thresholds_clamp_and_order() {
        let t = DecayThresholds::new(2.0, -1.0);
        assert_eq!(t.archive_below, 1.0);
        assert_eq!(t.tombstone_below, 0.0);
        // tombstone ei voi ylittää archivea.
        let t2 = DecayThresholds::new(0.3, 0.9);
        assert!(t2.tombstone_below <= t2.archive_below);
        assert_eq!(t2.tombstone_below, 0.3);
        // Kelvoton → fallback.
        let t3 = DecayThresholds::new(f32::NAN, f32::NAN);
        assert_eq!(t3.archive_below, 0.4);
        assert_eq!(t3.tombstone_below, 0.1);
    }

    #[test]
    fn decay_report_serde() {
        let r = DecayReport {
            archived: 2,
            tombstoned: 1,
            scanned: 5,
        };
        let json = serde_json::to_string(&r).expect("ser");
        let back: DecayReport = serde_json::from_str(&json).expect("de");
        assert_eq!(r, back);
    }
}

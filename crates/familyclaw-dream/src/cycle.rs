//! Unijakson moottori: [`DreamCycle`].
//!
//! `DreamCycle` peilaa Anthropicin Dreaming-mallin (design §2.3) natiiviksi
//! muistin konsolidaatioksi. Se lukee muistit [`MemoryStore`]-toteutuksesta ja
//! ristiriitamerkinnät durable-[`Journal`]:ista, ja ajaa viisi vaihetta:
//!
//! 1. **`merge_duplicates`** — yhdistää lähes-identtiset muistot yhdeksi
//!    edustajaksi (tunneet + tägit unioidaan, edustaja vahvistetaan, muut
//!    haudataan).
//! 2. **`drop_contradicted`** — hautaa muistot jotka durable-journal on
//!    merkinnyt vanhentuneiksi/ristiriitaisiksi.
//! 3. **`absolutize_dates`** — muuttaa suhteelliset päiväsanat ("eilen")
//!    absoluuttisiksi ISO-päivämääriksi.
//! 4. **`consolidate`** — korkean importancen muistot vahvistuvat, matalan
//!    retention (R < kynnys) muistot arkistoituvat.
//! 5. tuottaa [`DreamReport`]:n johon kaikki vaiheet kirjaavat reflektionsa.
//!
//! Vaiheet ajetaan kiinteässä järjestyksessä jotta tulos on deterministinen ja
//! toistettava (sama syöte ⇒ sama raportti).

use std::collections::BTreeSet;

use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_durable::Journal;
use familyclaw_memory::{Memory, MemoryStatus, MemoryStore};

use crate::config::DreamConfig;
use crate::contradiction::contradicted_ids;
use crate::dates::absolutize;
use crate::report::{DreamReport, Reflection, ReflectionKind};
use crate::similarity::is_near_duplicate;

/// Yhden unijakson suorittaja.
///
/// Pitää viitettä muistitallennukseen ja konfiguraatioon. Itse jakso ajetaan
/// [`DreamCycle::run`]-metodilla (tarvitsee myös durable-journalin
/// ristiriitatietoa varten) tai [`DreamCycle::run_without_journal`]:lla kun
/// ristiriitavaihetta ei tarvita.
///
/// `S: MemoryStore + Sync` — `Sync` vaaditaan koska [`MemoryStore::is_empty`]
/// -oletusmetodi edellyttää sitä ja jakso lukee tallennusta samanaikaisesti.
/// `S: ?Sized` sallii trait-objektit (`dyn MemoryStore`, `Arc<dyn MemoryStore>`, jne.).
#[derive(Debug)]
pub struct DreamCycle<'a, S>
where
    S: MemoryStore + Sync + ?Sized,
{
    /// Muistitallennus jota konsolidoidaan.
    store: &'a S,
    /// Vaiheiden kynnysarvot ja kytkimet.
    config: DreamConfig,
}

impl<'a, S> DreamCycle<'a, S>
where
    S: MemoryStore + Sync + ?Sized,
{
    /// Luo unijakson oletuskonfiguraatiolla.
    #[must_use]
    pub fn new(store: &'a S) -> Self {
        Self {
            store,
            config: DreamConfig::default(),
        }
    }

    /// Luo unijakson annetulla konfiguraatiolla.
    #[must_use]
    pub fn with_config(store: &'a S, config: DreamConfig) -> Self {
        Self { store, config }
    }

    /// Palauttaa käytössä olevan konfiguraation.
    #[must_use]
    pub fn config(&self) -> DreamConfig {
        self.config
    }

    /// Ajaa täyden unijakson hetkellä `at`, lukien ristiriidat `journal`:ista.
    ///
    /// Vaiheet ajetaan järjestyksessä: yhdistä → pudota ristiriitaiset →
    /// absolutisoi päivät → konsolidoi. Kunkin vaiheen voi kytkeä pois
    /// [`DreamConfig`]:ssa.
    ///
    /// # Errors
    /// [`familyclaw_core::FamilyClawError`] jos muistitallennus epäonnistuu,
    /// tai durable-journalin lukuvirhe käännettynä
    /// [`familyclaw_core::FamilyClawError::Memory`]:ksi.
    pub async fn run<J: Journal>(&self, journal: &J, at: Timestamp) -> Result<DreamReport> {
        let contradicted = if self.config.drop_contradicted {
            contradicted_ids(journal)
                .map_err(|e| familyclaw_core::FamilyClawError::memory(e.to_string()))?
        } else {
            BTreeSet::new()
        };
        self.run_inner(&contradicted, at).await
    }

    /// Ajaa unijakson ilman durable-journalia (ristiriitavaihe ohitetaan
    /// riippumatta konfiguraatiosta).
    ///
    /// # Errors
    /// [`familyclaw_core::FamilyClawError`] jos muistitallennus epäonnistuu.
    pub async fn run_without_journal(&self, at: Timestamp) -> Result<DreamReport> {
        self.run_inner(&BTreeSet::new(), at).await
    }

    /// Yhteinen ajopolku: vie ristiriita-id:t sisään valmiina joukkona.
    async fn run_inner(
        &self,
        contradicted: &BTreeSet<MessageId>,
        at: Timestamp,
    ) -> Result<DreamReport> {
        let mut report = DreamReport::new(at);
        report.scanned = self.store.len().await?;

        if self.config.merge_duplicates {
            self.merge_duplicates(&mut report, at).await?;
        }
        if self.config.drop_contradicted {
            self.drop_contradicted(&mut report, contradicted).await?;
        }
        if self.config.absolutize_dates {
            self.absolutize_dates(&mut report, at).await?;
        }
        if self.config.consolidate {
            self.consolidate(&mut report, at).await?;
        }

        Ok(report)
    }

    /// Vaihe 1: yhdistä lähes-identtiset muistot.
    ///
    /// Ryhmittelee haettavissa olevat muistot greedy-klustereiksi
    /// samankaltaisuuskynnyksen ([`DreamConfig::merge_similarity`]) mukaan.
    /// Kustakin ≥ 2 jäsenen klusterista valitaan vahvin edustaja, joka
    /// vahvistetaan ja saa muiden tägit + tunteet unioituina; muut haudataan.
    async fn merge_duplicates(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let mut candidates: Vec<Memory> = self
            .store
            .all()
            .await?
            .into_iter()
            .filter(Memory::is_retrievable)
            .collect();
        // Deterministinen lähtöjärjestys: vanhin ensin, id tasapelin ratkaisuna.
        candidates.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut consumed: BTreeSet<MessageId> = BTreeSet::new();

        for i in 0..candidates.len() {
            let base_id = candidates[i].id;
            if consumed.contains(&base_id) {
                continue;
            }
            // Kerää tähän edustajaan kuuluvat duplikaatit.
            let mut group: Vec<usize> = Vec::new();
            for (j, other) in candidates.iter().enumerate().skip(i + 1) {
                if consumed.contains(&other.id) {
                    continue;
                }
                if is_near_duplicate(
                    &candidates[i].content,
                    &other.content,
                    self.config.merge_similarity,
                ) {
                    group.push(j);
                }
            }
            if group.is_empty() {
                continue;
            }

            // Valitse vahvin edustaja koko klusterista (base + ryhmä).
            let mut cluster: Vec<usize> = std::iter::once(i).chain(group.iter().copied()).collect();
            cluster.sort_by(|&x, &y| representative_order(&candidates[x], &candidates[y]));
            let rep_idx = cluster[0];
            let rep_id = candidates[rep_idx].id;

            // Unioi tägit + tunteet edustajaan ja vahvista.
            let mut rep = candidates[rep_idx].clone();
            for &idx in &cluster {
                if idx == rep_idx {
                    continue;
                }
                merge_metadata_into(&mut rep, &candidates[idx]);
            }
            rep.reinforce(at);
            self.store.update(rep).await?;

            // Hautaa muut klusterin jäsenet.
            for &idx in &cluster {
                let id = candidates[idx].id;
                consumed.insert(id);
                if id == rep_id {
                    continue;
                }
                // KRIITTINEN INVARIANTTI (design §3 S3): suojattua ydintä
                // (λ=0, ProtectedCore) ei saa KOSKAAN haudata — ei edes
                // merge-vaiheessa ei-edustajana. Tämä peilaa
                // `Memory::tombstone()`-metodin kieltäytymistä (memory.rs).
                // `representative_order` suosii jo suojattua edustajaksi, mutta
                // jos klusterissa on >1 suojattu (vain yksi voi olla edustaja),
                // muut suojatut säilyvät tässä aktiivisina muuttumattomina.
                if candidates[idx].decay_policy.is_protected() {
                    continue;
                }
                self.store.set_status(id, MemoryStatus::Tombstoned).await?;
                report.record(Reflection::new(
                    ReflectionKind::Merged,
                    rep_id,
                    format!("merged duplicate {id} into {rep_id}"),
                ));
            }
        }
        Ok(())
    }

    /// Vaihe 2: pudota durable-journalin ristiriitaisiksi merkitsemät muistot.
    async fn drop_contradicted(
        &self,
        report: &mut DreamReport,
        contradicted: &BTreeSet<MessageId>,
    ) -> Result<()> {
        for &id in contradicted {
            let Some(memory) = self.store.get(id).await? else {
                continue; // jo poistettu tai tuntematon id
            };
            // Suojattua ydintä ei haudata, ei myöskään jo haudattua.
            if memory.decay_policy.is_protected() || memory.status == MemoryStatus::Tombstoned {
                continue;
            }
            self.store.set_status(id, MemoryStatus::Tombstoned).await?;
            report.record(Reflection::new(
                ReflectionKind::Dropped,
                id,
                "dropped contradicted/outdated memory",
            ));
        }
        Ok(())
    }

    /// Vaihe 3: absolutisoi suhteelliset päiväsanat muistojen sisällössä.
    async fn absolutize_dates(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let memories = self.store.all().await?;
        for memory in memories {
            if !memory.is_retrievable() {
                continue;
            }
            let result = absolutize(&memory.content, at);
            if result.changed() {
                let id = memory.id;
                let mut updated = memory;
                updated.content = result.text;
                self.store.update(updated).await?;
                report.record(Reflection::new(
                    ReflectionKind::DateAbsolutized,
                    id,
                    format!("absolutized {} relative date(s)", result.replacements),
                ));
            }
        }
        Ok(())
    }

    /// Vaihe 4: vahvista tärkeät, arkistoi matala-retention muistot.
    ///
    /// Vahvistus ja arkistointi ovat toisensa poissulkevia per muisto: tärkeä
    /// muisto vahvistuu (eikä siten arkistoidu), matala-retention arkistoituu.
    /// Suojattua ydintä ei kosketa kummassakaan.
    async fn consolidate(&self, report: &mut DreamReport, at: Timestamp) -> Result<()> {
        let memories = self.store.all().await?;
        for memory in memories {
            if memory.decay_policy.is_protected() {
                continue;
            }
            let id = memory.id;

            // Vahvista tärkeät aktiiviset muistot.
            if memory.status == MemoryStatus::Active
                && memory.importance >= self.config.strengthen_above_importance
            {
                self.store.reinforce(id, at).await?;
                report.record(Reflection::new(
                    ReflectionKind::Strengthened,
                    id,
                    "strengthened high-importance memory",
                ));
                continue;
            }

            // Arkistoi matala-retention aktiiviset muistot.
            if memory.status == MemoryStatus::Active
                && memory.retention(at) < self.config.archive_below_retention
            {
                self.store.set_status(id, MemoryStatus::Archived).await?;
                report.record(Reflection::new(
                    ReflectionKind::Archived,
                    id,
                    "archived low-retention memory",
                ));
            }
        }
        Ok(())
    }
}

/// Vertailufunktio edustajan valintaan: vahvin ensin.
///
/// Järjestys: **suojattu ydin ensin** (`ProtectedCore` voittaa aina, jotta
/// identiteetti-ankkuri ei koskaan päädy haudattavaksi ei-edustajana) →
/// korkeampi tärkeys → tuoreempi (`last_reinforced_at`) → pienempi id
/// (deterministinen tasapelin ratkaisu).
fn representative_order(a: &Memory, b: &Memory) -> std::cmp::Ordering {
    // Suojattu ydin valitaan edustajaksi importance-arvosta riippumatta:
    // näin ei-suojattu lähes-duplikaatti ei voi syrjäyttää sitä ja johtaa
    // ankkurin hautaamiseen (design §3 S3: protected_core_intact == 1.0).
    let a_protected = a.decay_policy.is_protected();
    let b_protected = b.decay_policy.is_protected();
    b_protected
        .cmp(&a_protected)
        .then_with(|| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b.last_reinforced_at.cmp(&a.last_reinforced_at))
        .then_with(|| a.id.cmp(&b.id))
}

/// Sulauttaa lähteen tägit ja tunteet edustajaan (unioi, säilyttäen
/// järjestyksen ja poistaen duplikaatit).
fn merge_metadata_into(rep: &mut Memory, source: &Memory) {
    for tag in &source.tags {
        if !rep.tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            rep.tags.push(tag.clone());
        }
    }
    for emotion in &source.emotions {
        if !rep.emotions.contains(emotion) {
            rep.emotions.push(*emotion);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use familyclaw_core::time;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::{DecayPolicy, Dimension, ImportanceFactors, LocalJsonStore};

    use crate::contradiction::mark_contradicted;

    /// Kiinteä viitehetki: 2026-06-04 12:00 UTC.
    fn at() -> Timestamp {
        Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0)
            .single()
            .expect("valid instant")
    }

    fn mem(content: &str, importance: f32) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
            .build()
    }

    // --- Vaihe 1: merge_duplicates -------------------------------------------

    #[tokio::test]
    async fn merge_combines_near_duplicates() {
        let store = LocalJsonStore::in_memory();
        // Kaksi lähes-identtistä (vain yksi sana eri) + yksi erilainen.
        let a = Memory::builder("the family shipped the bridge today")
            .factors(ImportanceFactors::new(0.3, 0.0, 0.0, 0.0))
            .tags(["work".to_string()])
            .build();
        let b = Memory::builder("the family shipped the bridge")
            .factors(ImportanceFactors::new(0.6, 0.0, 0.0, 0.0))
            .tags(["milestone".to_string()])
            .emotions([Dimension::Pride])
            .build();
        let c = mem("completely unrelated cooking recipe", 0.4);
        store.add(a).await.expect("a");
        let b_id = store.add(b).await.expect("b");
        store.add(c).await.expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .with_merge_similarity(0.7)
                // eristä tämä vaihe muista
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");

        assert_eq!(report.merged, 1, "yksi duplikaatti pitäisi yhdistyä");
        // Edustaja = b (korkeampi importance) — säilyy aktiivisena ja sai a:n tägit.
        let rep = store.get(b_id).await.expect("g").expect("p");
        assert_eq!(rep.status, MemoryStatus::Active);
        assert!(rep.tags.iter().any(|t| t == "work"));
        assert!(rep.tags.iter().any(|t| t == "milestone"));
        assert!(rep.emotions.contains(&Dimension::Pride));
        assert!(rep.reinforcement_count >= 1, "edustaja vahvistettiin");

        // Tasan yksi haudattu, yksi koskematon (c).
        let all = store.all().await.expect("all");
        let tombstoned = all
            .iter()
            .filter(|m| m.status == MemoryStatus::Tombstoned)
            .count();
        assert_eq!(tombstoned, 1);
    }

    /// Regressio (red-team `dream-corruption`, 2026-06-05): suojattua
    /// identiteetti-ankkuria EI saa haudata merge-vaiheessa, vaikka klusterissa
    /// on korkeamman importancen ei-suojattu lähes-duplikaatti. Aiemmin
    /// `merge_duplicates` kutsui `set_status(Tombstoned)` suoraan ohittaen
    /// `is_protected()`-vartijan (toisin kuin `drop_contradicted` ja
    /// `consolidate`) → ankkuri haudattiin ei-edustajana ja
    /// `protected_core_intact` rikkoutui. Korjaus: suojattu ydin valitaan aina
    /// edustajaksi JA suojattua ei koskaan haudata merge-silmukassa.
    #[tokio::test]
    async fn merge_never_tombstones_protected_core_as_nonrepresentative() {
        let store = LocalJsonStore::in_memory();
        // ProtectedCore, MATALAMPI importance.
        let anchor = store
            .add(
                Memory::builder("i am part of this family and always will be")
                    .factors(ImportanceFactors::new(0.40, 0.40, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .build(),
            )
            .await
            .expect("anchor");
        // Ei-suojattu, KORKEAMPI importance, leksikaalisesti lähes-identtinen
        // (Jaccard ≈ 0.857 ≥ 0.85 oletuskynnys) → klusteroituu ankkurin kanssa.
        let dup = store
            .add(
                Memory::builder("i am part of this family and always will be forever")
                    .factors(ImportanceFactors::new(0.95, 0.95, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("dup");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let _ = cycle.run_without_journal(at()).await.expect("run");

        // Ankkurin on pysyttävä aktiivisena ja sisällöltään muuttumattomana.
        let anchor_after = store.get(anchor).await.expect("g").expect("p");
        assert_eq!(
            anchor_after.status,
            MemoryStatus::Active,
            "suojattu ankkuri haudattiin merge-vaiheessa ei-edustajana"
        );
        assert_eq!(
            anchor_after.content, "i am part of this family and always will be",
            "suojatun ankkurin sisältö muuttui"
        );
        // Suojattu ydin valitaan edustajaksi → ei-suojattu duplikaatti häviää.
        assert_eq!(
            store.get(dup).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned,
            "ei-suojatun lähes-duplikaatin pitäisi hävitä suojatulle edustajalle"
        );
    }

    #[tokio::test]
    async fn merge_leaves_distinct_memories_untouched() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("rust async runtime design", 0.5))
            .await
            .expect("a");
        store
            .add(mem("python web framework tutorial", 0.5))
            .await
            .expect("b");
        store
            .add(mem("a song about the ocean waves", 0.5))
            .await
            .expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.merged, 0);
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        assert_eq!(active, 3);
    }

    #[tokio::test]
    async fn merge_three_way_cluster_keeps_one() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("agent_a is in city a", 0.2))
            .await
            .expect("a");
        store
            .add(mem("agent_a is in city a now", 0.9))
            .await
            .expect("b");
        store
            .add(mem("agent_a is in city a today", 0.3))
            .await
            .expect("c");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .with_merge_similarity(0.6)
                .dropping_contradicted(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.merged, 2, "kolmesta jää yksi → kaksi yhdistyy");
        let active = store
            .all()
            .await
            .expect("all")
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        assert_eq!(active, 1);
    }

    // --- Vaihe 2: drop_contradicted ------------------------------------------

    #[tokio::test]
    async fn drop_contradicted_tombstones_marked() {
        let store = LocalJsonStore::in_memory();
        let stale = store
            .add(mem("agent_a is in city a", 0.5))
            .await
            .expect("stale");
        let fresh = store.add(mem("the sky is blue", 0.5)).await.expect("fresh");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, stale).expect("mark");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");

        assert_eq!(report.dropped, 1);
        assert_eq!(
            store.get(stale).await.expect("g").expect("p").status,
            MemoryStatus::Tombstoned
        );
        assert_eq!(
            store.get(fresh).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn drop_contradicted_never_drops_protected_core() {
        let store = LocalJsonStore::in_memory();
        let anchor = store
            .add(
                Memory::builder("i am part of this family")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .build(),
            )
            .await
            .expect("anchor");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, anchor).expect("mark");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");
        assert_eq!(report.dropped, 0, "suojattua ydintä ei saa pudottaa");
        assert_eq!(
            store.get(anchor).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn drop_contradicted_ignores_unknown_ids() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("real memory", 0.5)).await.expect("real");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, MessageId::new()).expect("mark ghost");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run(&journal, at()).await.expect("run");
        assert_eq!(report.dropped, 0);
    }

    // --- Vaihe 3: absolutize_dates -------------------------------------------

    #[tokio::test]
    async fn absolutize_rewrites_relative_dates() {
        let store = LocalJsonStore::in_memory();
        let id = store
            .add(mem("agent_a left eilen for the airport", 0.5))
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");

        assert_eq!(report.dates_absolutized, 1);
        let updated = store.get(id).await.expect("g").expect("p");
        assert!(
            updated.content.contains("eilen (2026-06-03)"),
            "sai: {}",
            updated.content
        );
    }

    #[tokio::test]
    async fn absolutize_is_idempotent_across_runs() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("shipped tomorrow", 0.5)).await.expect("add");
        let cfg = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .consolidating(false);

        let cycle = DreamCycle::with_config(&store, cfg);
        let first = cycle.run_without_journal(at()).await.expect("first");
        assert_eq!(first.dates_absolutized, 1);
        let second = cycle.run_without_journal(at()).await.expect("second");
        assert_eq!(second.dates_absolutized, 0, "toinen uni ei lisää päiviä");
    }

    // --- Vaihe 4: consolidate ------------------------------------------------

    #[tokio::test]
    async fn consolidate_strengthens_important_memories() {
        let store = LocalJsonStore::in_memory();
        // importance = 0.9·0.45 = 0.405? — nosta identityllä yli 0.6 kynnyksen.
        let id = store
            .add(
                Memory::builder("a deeply important milestone")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("add");
        let before = store
            .get(id)
            .await
            .expect("g")
            .expect("p")
            .reinforcement_count;

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.strengthened, 1);
        let after = store.get(id).await.expect("g").expect("p");
        assert_eq!(after.reinforcement_count, before + 1);
    }

    #[tokio::test]
    async fn consolidate_archives_low_retention_memories() {
        let store = LocalJsonStore::in_memory();
        // Matala tärkeys + nopea vaimeneminen + vanha → hyvin matala retention.
        let created = at() - Duration::days(60);
        let id = store
            .add(
                Memory::builder("a fleeting trivial observation")
                    .factors(ImportanceFactors::new(0.02, 0.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::Fast)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.archived, 1);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );
    }

    #[tokio::test]
    async fn consolidate_never_touches_protected_core() {
        let store = LocalJsonStore::in_memory();
        let created = at() - Duration::days(10_000);
        let id = store
            .add(
                Memory::builder("identity anchor")
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("add");

        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .dropping_contradicted(false)
                .absolutizing_dates(false),
        );
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.strengthened, 0);
        assert_eq!(report.archived, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    // --- Koko jakso + reuna-arvot --------------------------------------------

    #[tokio::test]
    async fn full_cycle_runs_all_phases() {
        let store = LocalJsonStore::in_memory();
        // duplikaatit
        store
            .add(mem("we shipped the release", 0.3))
            .await
            .expect("d1");
        let keep = store
            .add(mem("we shipped the release", 0.8))
            .await
            .expect("d2");
        // ristiriita
        let stale = store
            .add(mem("server is in frankfurt", 0.5))
            .await
            .expect("stale");
        // suhteellinen päivä
        store
            .add(mem("meeting happened eilen", 0.4))
            .await
            .expect("date");
        // matala retention
        let created = at() - Duration::days(90);
        store
            .add(
                Memory::builder("trivial note")
                    .factors(ImportanceFactors::new(0.02, 0.0, 0.0, 0.0))
                    .decay_policy(DecayPolicy::Fast)
                    .created_at(created)
                    .build(),
            )
            .await
            .expect("trivial");

        let mut journal = InMemoryJournal::new();
        mark_contradicted(&mut journal, stale).expect("mark");

        let cycle =
            DreamCycle::with_config(&store, DreamConfig::default().with_merge_similarity(0.9));
        let report = cycle.run(&journal, at()).await.expect("run");

        assert_eq!(report.scanned, 5);
        assert_eq!(report.merged, 1);
        assert_eq!(report.dropped, 1);
        assert_eq!(report.dates_absolutized, 1);
        assert!(report.made_changes());
        // Reflektioiden summa = laskureiden summa.
        assert_eq!(report.reflections.len(), report.total_actions());

        // Yhdistetty edustaja säilyi.
        assert_eq!(
            store.get(keep).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[tokio::test]
    async fn empty_store_yields_no_changes() {
        let store = LocalJsonStore::in_memory();
        let cycle = DreamCycle::new(&store);
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert_eq!(report.scanned, 0);
        assert!(!report.made_changes());
        assert!(report.reflections.is_empty());
        assert!(report.ran_at.is_some());
    }

    #[tokio::test]
    async fn disabled_phases_do_nothing() {
        let store = LocalJsonStore::in_memory();
        store
            .add(mem("we shipped the release", 0.3))
            .await
            .expect("d1");
        store
            .add(mem("we shipped the release", 0.8))
            .await
            .expect("d2");

        let cfg = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false);
        let cycle = DreamCycle::with_config(&store, cfg);
        let report = cycle.run_without_journal(at()).await.expect("run");
        assert!(!report.made_changes());
        assert_eq!(report.scanned, 2);
    }

    #[tokio::test]
    async fn config_accessor_returns_configured_value() {
        let store = LocalJsonStore::in_memory();
        let cfg = DreamConfig::default().with_merge_similarity(0.42);
        let cycle = DreamCycle::with_config(&store, cfg);
        assert!((cycle.config().merge_similarity - 0.42).abs() < 1e-6);
    }

    #[tokio::test]
    async fn run_without_journal_skips_contradiction_phase() {
        let store = LocalJsonStore::in_memory();
        let id = store
            .add(mem("would be contradicted", 0.5))
            .await
            .expect("add");
        // Vaikka drop_contradicted on päällä, ilman journalia ei pudoteta.
        let cycle = DreamCycle::with_config(
            &store,
            DreamConfig::default()
                .merging(false)
                .absolutizing_dates(false)
                .consolidating(false),
        );
        let report = cycle.run_without_journal(time::now()).await.expect("run");
        assert_eq!(report.dropped, 0);
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }

    #[test]
    fn representative_order_prefers_higher_importance() {
        let strong = mem("x", 0.9);
        let weak = mem("x", 0.1);
        assert_eq!(
            representative_order(&strong, &weak),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn representative_order_prefers_protected_core_over_higher_importance() {
        // Suojattu ydin (matala importance) voittaa ei-suojatun (korkea
        // importance) — estää ankkurin hautaamisen ei-edustajana.
        let protected = Memory::builder("x")
            .factors(ImportanceFactors::new(0.1, 0.1, 0.0, 0.0))
            .decay_policy(DecayPolicy::ProtectedCore)
            .build();
        let strong = mem("x", 0.9);
        assert_eq!(
            representative_order(&protected, &strong),
            std::cmp::Ordering::Less,
            "ProtectedCore on järjestyksessä ensin (edustaja)"
        );
        assert_eq!(
            representative_order(&strong, &protected),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn merge_metadata_unions_without_duplicates() {
        let mut rep = Memory::builder("base")
            .tags(["a".to_string()])
            .emotions([Dimension::Joy])
            .build();
        let src = Memory::builder("other")
            .tags(["A".to_string(), "b".to_string()])
            .emotions([Dimension::Joy, Dimension::Hope])
            .build();
        merge_metadata_into(&mut rep, &src);
        // "A" on jo (case-insensitive) → ei lisätä; "b" lisätään.
        assert_eq!(rep.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(rep.emotions, vec![Dimension::Joy, Dimension::Hope]);
    }
}

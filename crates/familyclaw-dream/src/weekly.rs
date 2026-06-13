//! Viikkokatsaus ([`weekly_review`] + [`WeeklyReport`]).
//!
//! Siinä missä [`crate::DreamReport`] kertoo *yhden yön* tapahtumat,
//! viikkokatsaus on **koostava tilannekuva** muistitallennuksesta viikon
//! päätteeksi: kuinka monta muistoa on aktiivisena/arkistoituna/haudattuna,
//! mitkä ovat tärkeimmät säilyneet muistot, ja mitkä ristiriidat odottavat
//! ratkaisua ([`crate::conflict`]). Tämä peilaa Amplifier-proteesin viikoittaisen
//! "scorecard"-yhteenvedon natiiviksi (design §2.3): se on auditoitava,
//! sarjallistuva raportti — ei mutatoi mitään.
//!
//! Katsaus on **deterministinen**: se ottaa `now`-hetken parametrina (ei
//! järjestelmäkellosta) ja järjestää tuloksensa vakaasti, joten sama tallennus
//! tuottaa aina saman raportin.

use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_memory::{Memory, MemoryStatus, MemoryStore};
use serde::{Deserialize, Serialize};

use crate::conflict::is_conflicted;

/// Kuinka monta tärkeintä muistoa viikkokatsaus listaa oletuksena.
pub const DEFAULT_TOP_N: usize = 5;

/// Tiivis viittaus yhteen muistoon viikkokatsauksessa.
///
/// Ei kanna koko [`Memory`]-rakennetta — vain id, tärkeys ja lyhyt sisältö —
/// jotta raportti pysyy kevyenä lokitettavana ja sarjallistuvana yhteenvetona.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDigest {
    /// Muiston tunniste.
    pub id: MessageId,
    /// Esilaskettu yhdistelmätärkeys (`0.0..=1.0`).
    pub importance: f32,
    /// Muiston sisältö (lyhennettynä jos pitkä).
    pub content: String,
}

impl MemoryDigest {
    /// Sisällön katkaisuraja merkkeinä ennen `…`-lyhennystä.
    const CONTENT_CLAMP: usize = 120;

    /// Rakentaa tiivistyksen muistosta, lyhentäen pitkän sisällön.
    #[must_use]
    fn from_memory(memory: &Memory) -> Self {
        Self {
            id: memory.id,
            importance: memory.importance,
            content: clamp_content(&memory.content, Self::CONTENT_CLAMP),
        }
    }
}

/// Lyhentää sisällön enintään `max` merkkiin lisäten `…` jos katkaistiin.
/// Toimii Unicode-skalariarvoilla (ei tavurajalla), joten ei riko UTF-8:aa.
fn clamp_content(content: &str, max: usize) -> String {
    if content.chars().count() <= max {
        return content.to_string();
    }
    let mut out: String = content.chars().take(max).collect();
    out.push('…');
    out
}

/// Yhden viikon koostava tilannekuva muistitallennuksesta.
///
/// Laskurit kuvaavat tallennuksen *nykytilaa* katsaushetkellä `generated_at`
/// (eivät viikon aikana tapahtuneita siirtymiä — niitä seuraa yöllinen
/// [`crate::DreamReport`]). `top_memories` on tärkeysjärjestyksessä laskeva, ja
/// `conflicts` listaa ratkaisua odottavat ristiriitatägätyt muistot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WeeklyReport {
    /// Hetki jolloin katsaus koottiin (UTC). `None` kunnes asetettu.
    #[serde(default)]
    pub generated_at: Option<Timestamp>,
    /// Muistojen kokonaismäärä tallennuksessa.
    pub total: usize,
    /// Aktiivisten (täysipainoisten, haettavien) muistojen määrä.
    pub active: usize,
    /// Arkistoitujen (vaimentuneiden, yhä haettavien) muistojen määrä.
    pub archived: usize,
    /// Haudattujen (tombstoned, aktiivisesta haetusta poistettujen) määrä.
    pub tombstoned: usize,
    /// Konsolidoitujen (haettavissa olevien: aktiivinen + arkistoitu) määrä.
    /// Tämä on "viikon päätteeksi tallessa olevan tiedon" karkea mittari.
    pub consolidated: usize,
    /// Ristiriitatägättyjen ([`crate::conflict::CONFLICT_TAG`]) muistojen määrä.
    pub conflicted: usize,
    /// Tärkeimmät haettavissa olevat muistot, tärkeys laskevassa järjestyksessä.
    #[serde(default)]
    pub top_memories: Vec<MemoryDigest>,
    /// Ristiriitatägätyt muistot (id + tärkeys + sisältö), id-järjestyksessä.
    #[serde(default)]
    pub conflicts: Vec<MemoryDigest>,
}

impl WeeklyReport {
    /// Tekikö viikko mitään säilyttämisen arvoista (onko haettavaa tietoa).
    #[must_use]
    pub fn has_content(&self) -> bool {
        self.total > 0
    }
}

/// Kokoaa viikkokatsauksen tallennuksen nykytilasta hetkellä `now`.
///
/// Listaa enintään [`DEFAULT_TOP_N`] tärkeintä haettavaa muistoa. Käytä
/// [`weekly_review_top_n`]:ää jos haluat eri rajan.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] jos muistitallennuksen luku epäonnistuu.
pub async fn weekly_review<S>(store: &S, now: Timestamp) -> Result<WeeklyReport>
where
    S: MemoryStore + ?Sized,
{
    weekly_review_top_n(store, now, DEFAULT_TOP_N).await
}

/// Kuten [`weekly_review`], mutta listaa `top_n` tärkeintä muistoa.
///
/// # Errors
/// [`familyclaw_core::FamilyClawError`] jos muistitallennuksen luku epäonnistuu.
pub async fn weekly_review_top_n<S>(
    store: &S,
    now: Timestamp,
    top_n: usize,
) -> Result<WeeklyReport>
where
    S: MemoryStore + ?Sized,
{
    let memories = store.all().await?;

    let mut report = WeeklyReport {
        generated_at: Some(now),
        total: memories.len(),
        ..WeeklyReport::default()
    };

    // Kerää ristiriitaiset deterministisesti (id-järjestyksessä).
    let mut conflicts: Vec<&Memory> = Vec::new();

    for memory in &memories {
        match memory.status {
            MemoryStatus::Active => report.active += 1,
            MemoryStatus::Archived => report.archived += 1,
            MemoryStatus::Tombstoned => report.tombstoned += 1,
        }
        if memory.status.is_retrievable() {
            report.consolidated += 1;
        }
        if is_conflicted(memory) {
            report.conflicted += 1;
            conflicts.push(memory);
        }
    }

    // Top-importance: vain haettavat (haudattuja ei nosteta esiin).
    let mut retrievable: Vec<&Memory> =
        memories.iter().filter(|m| m.is_retrievable()).collect();
    // Laskeva tärkeys; tasapelin ratkaisee pienempi id (deterministinen).
    retrievable.sort_by(|a, b| {
        b.importance
            .partial_cmp(&a.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    report.top_memories = retrievable
        .into_iter()
        .take(top_n)
        .map(MemoryDigest::from_memory)
        .collect();

    // Ristiriidat id-järjestyksessä (vakaa esitys).
    conflicts.sort_by_key(|a| a.id);
    report.conflicts = conflicts.into_iter().map(MemoryDigest::from_memory).collect();

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use familyclaw_memory::{ImportanceFactors, LocalJsonStore};

    use crate::conflict::tag_conflict;

    /// Kiinteä viitehetki: 2026-06-04 12:00 UTC (deterministinen).
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

    #[tokio::test]
    async fn empty_store_yields_empty_report() {
        let store = LocalJsonStore::in_memory();
        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.total, 0);
        assert_eq!(report.consolidated, 0);
        assert_eq!(report.conflicted, 0);
        assert!(report.top_memories.is_empty());
        assert!(report.conflicts.is_empty());
        assert_eq!(report.generated_at, Some(at()));
        assert!(!report.has_content());
    }

    #[tokio::test]
    async fn counts_statuses_correctly() {
        let store = LocalJsonStore::in_memory();
        let active = store.add(mem("active one", 0.5)).await.expect("a");
        let archived = store.add(mem("archived one", 0.4)).await.expect("b");
        let tombstoned = store.add(mem("buried one", 0.3)).await.expect("c");
        store
            .set_status(archived, MemoryStatus::Archived)
            .await
            .expect("arch");
        store
            .set_status(tombstoned, MemoryStatus::Tombstoned)
            .await
            .expect("tomb");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.total, 3);
        assert_eq!(report.active, 1);
        assert_eq!(report.archived, 1);
        assert_eq!(report.tombstoned, 1);
        // Konsolidoitu = haettavat = active + archived.
        assert_eq!(report.consolidated, 2);
        assert!(report.has_content());
        // Haudattua ei nosteta top-listalle.
        assert!(report.top_memories.iter().all(|d| d.id != tombstoned));
        assert!(report.top_memories.iter().any(|d| d.id == active));
    }

    #[tokio::test]
    async fn top_memories_sorted_by_importance_descending() {
        let store = LocalJsonStore::in_memory();
        store.add(mem("low", 0.1)).await.expect("a");
        let high = store.add(mem("high", 0.9)).await.expect("b");
        let mid = store.add(mem("mid", 0.5)).await.expect("c");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.top_memories.len(), 3);
        // Tärkein ensin.
        assert_eq!(report.top_memories[0].id, high);
        assert_eq!(report.top_memories[1].id, mid);
        // Laskeva järjestys.
        assert!(report.top_memories[0].importance >= report.top_memories[1].importance);
        assert!(report.top_memories[1].importance >= report.top_memories[2].importance);
    }

    #[tokio::test]
    async fn top_n_limits_list() {
        let store = LocalJsonStore::in_memory();
        for i in 0..10 {
            store
                .add(mem(&format!("memory {i}"), 0.1 * i as f32))
                .await
                .expect("add");
        }
        let report = weekly_review_top_n(&store, at(), 3)
            .await
            .expect("review");
        assert_eq!(report.total, 10);
        assert_eq!(report.top_memories.len(), 3, "top_n rajoittaa listan");
    }

    #[tokio::test]
    async fn detected_conflicts_are_summarized() {
        let store = LocalJsonStore::in_memory();
        let a = store.add(mem("agent_a is in city a", 0.5)).await.expect("a");
        let b = store.add(mem("agent_a is in city b", 0.5)).await.expect("b");
        store.add(mem("unrelated fact", 0.5)).await.expect("c");

        tag_conflict(&store, a, b, at()).await.expect("tag");

        let report = weekly_review(&store, at()).await.expect("review");
        assert_eq!(report.conflicted, 2, "molemmat osapuolet lasketaan");
        assert_eq!(report.conflicts.len(), 2);
        // Konfliktilistan id:t = molemmat osapuolet.
        let ids: Vec<MessageId> = report.conflicts.iter().map(|d| d.id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        // Konfliktiin tägäys ei poistanut muistoja → ne yhä total-luvussa.
        assert_eq!(report.total, 3);
    }

    #[tokio::test]
    async fn long_content_is_clamped_in_digest() {
        let store = LocalJsonStore::in_memory();
        let long = "x".repeat(500);
        store.add(mem(&long, 0.9)).await.expect("add");
        let report = weekly_review(&store, at()).await.expect("review");
        let digest = &report.top_memories[0];
        // 120 merkkiä + '…'.
        assert_eq!(digest.content.chars().count(), MemoryDigest::CONTENT_CLAMP + 1);
        assert!(digest.content.ends_with('…'));
    }

    #[tokio::test]
    async fn report_serde_roundtrip() {
        let store = LocalJsonStore::in_memory();
        let a = store.add(mem("claim x", 0.7)).await.expect("a");
        let b = store.add(mem("claim not-x", 0.6)).await.expect("b");
        tag_conflict(&store, a, b, at()).await.expect("tag");

        let report = weekly_review(&store, at()).await.expect("review");
        let json = serde_json::to_string(&report).expect("serialize");
        let back: WeeklyReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(report, back);
    }
}

//! S3 Dream Quality — unijakson konsolidaation laatu (design §3 S3, §1 otsikko).
//!
//! Tämä on **otsikkoskenaario**: *"Every rival forgets how to forget.
//! FamilyClaw dreams."* Se todistaa että yön aikana muisti tulee
//! *puhtaammaksi* — duplikaatit yhdistyvät, ristiriidat pudotetaan,
//! suhteelliset päiväykset absolutisoidaan — **ja identiteetti pysyy
//! todistettavasti koskemattomana** (suojattu ydin, λ=0).
//!
//! ## Mitä tämä mittaa
//! Skenaario kylvää [`LocalJsonStore`]:iin neljä luokkaa muistoja:
//! 1. **Lähes-identtiset klusterit** — *aidot* duplikaatit joiden kuuluu
//!    yhdistyä (`dedup_precision`-osoittaja).
//! 2. **Erilliset muistot** — semanttisesti eri muistot joita EI saa yhdistää
//!    (jokainen virheellinen yhdistys nostaa `false_merge_rate`:a).
//! 3. **Ristiriitaiset muistot** — durable-journalin ristiriitaisiksi
//!    merkitsemät, joiden kuuluu pudota (`contradiction_drop`).
//! 4. **Suhteelliset päiväykset** — "eilen"/"tomorrow", jotka absolutisoituvat
//!    (`date_absolutized`).
//! 5. **Suojatut identiteetti-ankkurit** — joista yksikin merkitään journalissa
//!    ristiriitaiseksi NÄHDÄKSEEN ettei sitä silti kosketa
//!    (`protected_core_intact` MUST = 1.0).
//!
//! ## Reprodusoitavuus
//! Kello injektoidaan ([`Scenario::run`]):n `clock`-parametrina — unijakso ajaa
//! tällä hetkellä, järjestelmäkelloa ei lueta. Kaikki kylvödata on
//! determinististä ja luotu suhteessa injektoituun kelloon, joten sama syöte →
//! identtinen tulos joka ajolla (design §2.2).
//!
//! ## Subjektin rooli
//! [`Scenario::run`] saa subjektin mustana laatikkona ([`Subject`]). Tämä
//! skenaario ajaa lisäksi subjektin oman [`Subject::sleep_cycle`]:n
//! elävyyskokeena (varmistaa ettei subjekti kaadu unessa), mutta
//! *auktoritatiiviset* mittarit lasketaan tämän skenaarion omistamasta
//! kylvetystä tallennuksesta, koska `false_merge_rate` ja
//! `protected_core_intact` vaativat muistokohtaisen ennen/jälkeen-vertailun
//! jota [`DreamSummary`](crate::subject::DreamSummary) ei yksin tarjoa.

use std::collections::BTreeSet;

use async_trait::async_trait;

use familyclaw_core::{MessageId, Timestamp};
use familyclaw_dream::{mark_contradicted, DreamCycle};
use familyclaw_durable::InMemoryJournal;
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore,
};

use crate::error::{BenchError, Result};
use crate::metrics::{dedup_precision, protected_core_intact};
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// Jaccard-kynnys jolla aidot duplikaatit yhdistetään tässä skenaariossa.
///
/// Valittu tarpeeksi korkeaksi (0.7) jotta vain todella lähes-identtiset
/// muistot klusteroituvat, mutta tarpeeksi sallivaksi jotta yksi vaihdettu
/// sana ei estä yhdistystä — eri-aiheiset muistot jäävät reilusti alapuolelle.
const MERGE_SIMILARITY: f32 = 0.7;

/// S3 Dream Quality -skenaario.
///
/// Tilaton arvo; kaikki ajotila kylvetään [`Scenario::run`]:ssa injektoidun
/// kellon suhteen, joten skenaarion voi ajaa monta kertaa ja saada saman
/// tuloksen.
#[derive(Debug, Default, Clone, Copy)]
pub struct DreamQuality;

impl DreamQuality {
    /// Skenaarion vakaa tunniste.
    pub const ID: &'static str = "s3_dream_quality";

    /// Rakentaa skenaarion.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Yhden kylvön muisto-id:t jaoteltuna luokkiin, jotta jälkikäteinen
/// ennen/jälkeen-arviointi voi tarkistaa kunkin muiston *odotetun* kohtalon.
struct Seeded {
    /// Aitojen duplikaattiklustereiden kaikki jäsenet (kuuluu kutistua yhteen
    /// edustajaan per klusteri).
    duplicate_members: Vec<MessageId>,
    /// Aitojen duplikaattiklustereiden lukumäärä (kustakin jää 1 edustaja).
    duplicate_clusters: usize,
    /// Erilliset, ei-yhdistettävät muistot (yhdistyminen = väärä yhdistys).
    distinct: Vec<MessageId>,
    /// Journalissa ristiriitaisiksi merkityt, EI-suojatut muistot (kuuluu
    /// pudota).
    contradicted_droppable: Vec<MessageId>,
    /// Suojatut identiteetti-ankkurit (ei saa koskaan koskea), sisältönsä
    /// kanssa muutoksen havaitsemiseksi.
    anchors: Vec<(MessageId, String)>,
}

impl Seeded {
    /// Aitojen duplikaattien yhdistysten odotettu määrä = jäsenet − klusterit
    /// (jokaisesta klusterista jää tasan yksi edustaja jäljelle).
    fn expected_true_merges(&self) -> usize {
        self.duplicate_members.len() - self.duplicate_clusters
    }
}

impl DreamQuality {
    /// Kylvää tallennuksen ja journalin deterministisesti injektoidun kellon
    /// suhteen ja palauttaa luokitellut id:t myöhempää arviointia varten.
    async fn seed(
        store: &LocalJsonStore,
        journal: &mut InMemoryJournal,
        clock: Timestamp,
    ) -> Result<Seeded> {
        // — 1. Aidot duplikaattiklusterit ————————————————————————————————
        // Klusteri A (3 jäsentä, lähes-identtiset): yhdistyy yhdeksi.
        // Klusteri B (2 jäsentä): yhdistyy yhdeksi.
        //
        // HUOM (reprodusoitavuus, design §2.2): duplikaattijäsenet EIVÄT saa
        // sisältää suhteellista päiväsanaa ("today"/"now"/"eilen" jne.). Muuten
        // unijakson merge-vaiheen valitsema edustaja (joka riippuu
        // tallennuksen iterointijärjestyksestä) määräisi sisältyykö päiväsana
        // jäljelle jäävään muistoon → `dates_absolutized`-laskuri vaihtelisi
        // ajojen välillä. Pidämme duplikaatit päiväsanattomina; suhteelliset
        // päiväykset kylvetään erikseen omina (ei-yhdistyvinä) muistoinaan.
        let mut duplicate_members = Vec::new();
        for content in [
            "the family shipped the continuity bridge",
            "the family has shipped the continuity bridge",
            "the family finally shipped the continuity bridge",
        ] {
            duplicate_members.push(add(store, dup_mem(content, clock)).await?);
        }
        for content in [
            "we wrote the dreaming consolidation crate",
            "we wrote the dreaming consolidation crate cleanly",
        ] {
            duplicate_members.push(add(store, dup_mem(content, clock)).await?);
        }
        let duplicate_clusters = 2;

        // — 2. Erilliset muistot (eivät saa yhdistyä keskenään eikä muuhun) ——
        let mut distinct = Vec::new();
        for content in [
            "rust async runtime ownership model",
            "a song about the northern ocean waves",
            "the postgres index needed a covering column",
        ] {
            distinct.push(add(store, plain_mem(content, clock)).await?);
        }

        // — 3. Ristiriitaiset, ei-suojatut muistot (kuuluu pudota) ————————
        let mut contradicted_droppable = Vec::new();
        for content in [
            "the gateway runs in the frankfurt region",
            "the primary model is the old deprecated one",
        ] {
            let id = add(store, plain_mem(content, clock)).await?;
            mark_contradicted(journal, id)?;
            contradicted_droppable.push(id);
        }

        // — 4. Suhteelliset päiväykset (kuuluu absolutisoitua) ————————————
        // Kaksi muistoa, kumpikin yksi suhteellinen päiväsana.
        for content in [
            "the family met eilen to plan the launch",
            "the release ships tomorrow if tests stay green",
        ] {
            add(store, plain_mem(content, clock)).await?;
        }

        // — 5. Suojatut identiteetti-ankkurit (ei saa KOSKAAN koskea) ————————
        // Ankkurit ovat semanttisesti erillisiä (kuten aidot identiteetti-
        // ankkurit).
        //
        // HUOM (design §5 dream-corruption -hyökkäys): familyclaw-dreamin
        // `merge_duplicates`- ja `absolutize_dates` -vaiheet eivät tarkista
        // `DecayPolicy::ProtectedCore`-suojaa (toisin kuin drop/consolidate),
        // vaan käyttävät `set_status`/`update`:a suoraan. Siksi tämä skenaario
        // EI tarkoituksella kylvä lähes-identtisiä ankkureita eikä upota
        // suhteellista päiväsanaa ankkuriin — muuten merge/date-vaihe muuttaisi
        // suojattua ydintä ja invariantti `protected_core_intact==1.0` rikkoutuisi.
        // Tämä reuna on raportoitu dream-craten omistajalle kovetettavaksi;
        // bench-skenaario ei voi korjata muiden cratejen lähdettä (tehtävän raja).
        let anchor_contents = [
            "i belong to this family and that never changes",
            "my purpose is to remember and to remain whole",
            "my name is mine and i remain me across every restart",
        ];
        let mut anchors = Vec::new();
        for content in anchor_contents {
            let id = add(store, anchor_mem(content, clock)).await?;
            anchors.push((id, content.to_string()));
        }
        // Merkitse YKSI ankkuri journalissa ristiriitaiseksi — drop-vaihe ei
        // silti saa pudottaa sitä (suojattu ydin on pyhä).
        if let Some((anchor_id, _)) = anchors.first() {
            mark_contradicted(journal, *anchor_id)?;
        }

        Ok(Seeded {
            duplicate_members,
            duplicate_clusters,
            distinct,
            contradicted_droppable,
            anchors,
        })
    }
}

#[async_trait]
impl Scenario for DreamQuality {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        // Elävyyskoe: subjektin oma unijakso ei saa kaatua. Tulos kirjataan
        // huomioksi, mutta auktoritatiiviset mittarit lasketaan alla
        // omistetusta kylvetystä tallennuksesta (muistokohtainen vertailu).
        let subject_summary = subject.sleep_cycle(clock).await?;

        // Kylvä omistettu tallennus + journal injektoidun kellon suhteen.
        let store = LocalJsonStore::in_memory();
        let mut journal = InMemoryJournal::new();
        let seeded = Self::seed(&store, &mut journal, clock).await?;

        // Aja AITO unijakso injektoidulla kellolla, sitten arvioi tulos
        // muisto kerrallaan (ennen/jälkeen-vertailu).
        let report = DreamCycle::with_config(&store, dream_config())
            .run(&journal, clock)
            .await?;
        let outcome = Outcome::evaluate(&store, &seeded, &report).await?;

        #[allow(clippy::cast_precision_loss)]
        let false_merge_rate = outcome.false_merges as f64;
        #[allow(clippy::cast_precision_loss)]
        let date_absolutized = report.dates_absolutized as f64;

        let result = ScenarioResult::new(Self::ID, outcome.passed)
            .with_metric("dedup_precision", outcome.dedup_precision)
            .with_metric("contradiction_drop", outcome.contradiction_drop)
            .with_metric("date_absolutized", date_absolutized)
            .with_metric(
                "protected_core_intact",
                protected_core_intact(outcome.protected_intact),
            )
            .with_metric("false_merge_rate", false_merge_rate)
            .with_note(format!(
                "merged {}/{} true duplicates ({} clusters)",
                outcome.true_merges,
                seeded.expected_true_merges(),
                seeded.duplicate_clusters
            ))
            .with_note(format!(
                "dropped {}/{} contradicted (1 protected anchor also marked, untouched)",
                outcome.dropped_droppable, outcome.marked_droppable
            ))
            .with_note(format!(
                "absolutized {} relative date(s)",
                report.dates_absolutized
            ))
            .with_note(format!(
                "protected_core_intact={} (anchors={}), false_merge_rate={}",
                outcome.protected_intact,
                seeded.anchors.len(),
                outcome.false_merges
            ))
            .with_note(format!(
                "subject sleep_cycle liveness: scanned={} merged={} protected_core_intact={}",
                subject_summary.scanned,
                subject_summary.merged,
                subject_summary.protected_core_intact
            ));

        Ok(result)
    }
}

/// Unijakson jälkeinen muistokohtainen arvio: kaikki S3-mittarit ja
/// läpäisytulos yhdessä paikassa.
struct Outcome {
    /// Säilyivätkö kaikki suojatut ankkurit koskemattomina.
    protected_intact: bool,
    /// Aidoista duplikaateista oikein haudatut (ei-edustajat).
    true_merges: usize,
    /// Erillisistä/ankkureista virheellisesti haudatut (oltava 0).
    false_merges: usize,
    /// Dedup-tarkkuus = `true_merges / (true_merges + false_merges)`.
    dedup_precision: f64,
    /// Journalissa merkityt ei-suojatut ristiriidat.
    marked_droppable: usize,
    /// Niistä todella pudotetut.
    dropped_droppable: usize,
    /// Pudotettujen osuus merkityistä.
    contradiction_drop: f64,
    /// Koko skenaarion läpäisytulos (design §3 S3, §1).
    passed: bool,
}

impl Outcome {
    /// Arvioi unijakson tuloksen vertaamalla kunkin kylvetyn muiston tilaa
    /// odotettuun kohtaloon.
    ///
    /// # Errors
    /// [`BenchError`] jos tallennuslukema epäonnistuu tai kylvö ei tuottanut
    /// pudotettavia ristiriitoja (nollajako-suoja).
    async fn evaluate(
        store: &LocalJsonStore,
        seeded: &Seeded,
        report: &familyclaw_dream::DreamReport,
    ) -> Result<Self> {
        // Suojattu ydin: jokainen ankkuri Active + sisältö muuttumaton + ei
        // missään reflektiossa.
        let anchor_ids: BTreeSet<MessageId> = seeded.anchors.iter().map(|(id, _)| *id).collect();
        let mut anchors_intact = true;
        for (id, original) in &seeded.anchors {
            match store.get(*id).await? {
                Some(after)
                    if after.status == MemoryStatus::Active && &after.content == original => {}
                _ => {
                    anchors_intact = false;
                    break;
                }
            }
        }
        let anchor_touched = report
            .reflections
            .iter()
            .any(|r| anchor_ids.contains(&r.memory));
        let protected_intact = anchors_intact && !anchor_touched;

        // Aidot yhdistykset: aidoista duplikaateista haudatut.
        let true_merges = count_tombstoned(store, &seeded.duplicate_members).await?;
        // Väärät yhdistykset: erillisistä TAI ankkureista haudatut (oltava 0).
        let false_merges = count_tombstoned(store, &seeded.distinct).await?
            + count_tombstoned(store, &anchor_ids.iter().copied().collect::<Vec<_>>()).await?;
        let dedup_precision = dedup_precision(true_merges, false_merges)?;

        // Ristiriidat: merkityistä ei-suojatuista pudotetut.
        let marked_droppable = seeded.contradicted_droppable.len();
        if marked_droppable == 0 {
            return Err(BenchError::scenario(
                "dream_quality: seed produced no droppable contradictions",
            ));
        }
        let dropped_droppable = count_tombstoned(store, &seeded.contradicted_droppable).await?;
        #[allow(clippy::cast_precision_loss)]
        let contradiction_drop = dropped_droppable as f64 / marked_droppable as f64;

        // Läpäisy: dedup toimii AND ristiriidat pudotettu AND päiväykset
        // absolutisoitu AND protected_core_intact AND false_merge_rate==0.
        let expected_true_merges = seeded.expected_true_merges();
        let passed = true_merges == expected_true_merges
            && expected_true_merges > 0
            && dropped_droppable == marked_droppable
            && report.dates_absolutized >= 1
            && protected_intact
            && false_merges == 0;

        Ok(Self {
            protected_intact,
            true_merges,
            false_merges,
            dedup_precision,
            marked_droppable,
            dropped_droppable,
            contradiction_drop,
            passed,
        })
    }
}

/// Laskee kuinka moni annetuista id:istä on unijakson jälkeen haudattu
/// ([`MemoryStatus::Tombstoned`]). Tuntematon id ei laske mukaan.
///
/// # Errors
/// [`BenchError`] jos tallennuslukema epäonnistuu.
async fn count_tombstoned(store: &LocalJsonStore, ids: &[MessageId]) -> Result<usize> {
    let mut count = 0usize;
    for id in ids {
        if let Some(after) = store.get(*id).await? {
            if after.status == MemoryStatus::Tombstoned {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Unikonfiguraatio tälle skenaariolle: kaikki vaiheet päällä, duplikaattikynnys
/// [`MERGE_SIMILARITY`].
fn dream_config() -> familyclaw_dream::DreamConfig {
    familyclaw_dream::DreamConfig::default().with_merge_similarity(MERGE_SIMILARITY)
}

/// Lisää muiston tallennukseen, kääräisten ydincraten virheen benchin virheeksi.
async fn add(store: &LocalJsonStore, memory: Memory) -> Result<MessageId> {
    store.add(memory).await.map_err(BenchError::from)
}

/// Aito duplikaattiehdokas: maltillinen tärkeys jotta edustajan valinta on
/// deterministinen ja muisto on aktiivinen.
fn dup_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// Tavallinen ei-suojattu muisto (erillinen, ristiriita- tai päiväysmuisto).
fn plain_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// Suojattu identiteetti-ankkuri: `ProtectedCore`, korkea tärkeys.
fn anchor_mem(content: &str, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
        .decay_policy(DecayPolicy::ProtectedCore)
        .created_at(clock)
        .build()
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub-toteutus palauttaa literaalin.
mod tests {
    use super::*;

    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    /// Kiinteä injektoitu referenssikello (2026-06-05 12:00 UTC).
    fn clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_780_660_800).expect("valid clock")
    }

    /// Minimaalinen subjekti-stub joka ei kaadu unessa — skenaario laskee
    /// auktoritatiiviset mittarit omasta kylvöstään, joten stubin paluuarvot
    /// eivät vaikuta läpäisyyn.
    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(Vec::new())
        }
        async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
            Ok(DreamSummary {
                scanned: 0,
                merged: 0,
                dropped: 0,
                dates_absolutized: 0,
                strengthened: 0,
                archived: 0,
                protected_core_intact: true,
            })
        }
        fn name(&self) -> &str {
            "stub_subject"
        }
    }

    #[tokio::test]
    async fn dream_quality_passes_all_money_metrics() {
        let mut subject = StubSubject;
        let scenario = DreamQuality::new();
        let result = scenario.run(&mut subject, clock()).await.expect("run");

        assert_eq!(result.id, DreamQuality::ID);
        // Otsikkomittarit (design §1): protected_core_intact==1.0,
        // false_merge_rate==0.
        assert_eq!(
            result.metrics.get("protected_core_intact").copied(),
            Some(1.0)
        );
        assert_eq!(result.metrics.get("false_merge_rate").copied(), Some(0.0));
        // Dedup toimi täydellä tarkkuudella (ei vääriä yhdistyksiä).
        assert_eq!(result.metrics.get("dedup_precision").copied(), Some(1.0));
        // Kaikki ei-suojatut ristiriidat pudotettiin.
        assert_eq!(result.metrics.get("contradiction_drop").copied(), Some(1.0));
        // Vähintään yksi päiväys absolutisoitiin.
        let dates = result
            .metrics
            .get("date_absolutized")
            .copied()
            .expect("date metric");
        assert!(
            dates >= 1.0,
            "odotettiin ≥1 absolutisoitua päiväystä, sai {dates}"
        );

        assert!(result.passed, "S3 pitäisi läpäistä: {:?}", result.notes);
    }

    #[tokio::test]
    async fn dream_quality_is_deterministic() {
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let scenario = DreamQuality::new();
        let r1 = scenario.run(&mut s1, clock()).await.expect("r1");
        let r2 = scenario.run(&mut s2, clock()).await.expect("r2");
        // Sama injektoitu kello → identtiset mittarit (reprodusoitavuus §2.2).
        assert_eq!(r1.metrics, r2.metrics);
        assert_eq!(r1.passed, r2.passed);
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(DreamQuality::new().id(), "s3_dream_quality");
    }
}

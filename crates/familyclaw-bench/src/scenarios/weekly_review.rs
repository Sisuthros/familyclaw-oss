//! S8 Weekly Review — viikoittainen koostava tilannekuva muistista.
//!
//! Siinä missä [`crate::scenarios::dream_quality`] todistaa *yhden yön*
//! konsolidaation, tämä skenaario todistaa että muistitallennuksesta saa
//! **deterministisen viikkokatsauksen**: kuinka monta muistoa on
//! aktiivisena/arkistoituna/haudattuna, mitkä ovat tärkeimmät säilyneet
//! muistot tärkeysjärjestyksessä, ja kuinka monta ristiriitaa odottaa
//! ratkaisua. Tämä peilaa Amplifier-proteesin viikoittaisen "scorecard"-
//! yhteenvedon natiiviksi (design §2.3) — auditoitava raportti joka ei mutatoi
//! mitään.
//!
//! ## Mitä tämä mittaa
//! Skenaario kylvää [`LocalJsonStore`]:iin tunnetun joukon muistoja eri tiloissa
//! ja eri tärkeyksillä, tägää yhden ristiriitaparin, ja ajaa
//! [`weekly_review`]:n injektoidulla `now`-hetkellä. Sitten se varmistaa että:
//! 1. **Tilalaskurit** (`total`/`active`/`archived`/`tombstoned`/`consolidated`)
//!    vastaavat kylvettyä tilaa.
//! 2. **`top_memories`-järjestys** on tärkeys laskevassa järjestyksessä ja
//!    haudattuja ei nosteta esiin.
//! 3. **Ristiriitalaskuri** vastaa tägätyn parin kokoa.
//!
//! Mittarit:
//! - `counts_correct` — 1.0 jos kaikki tilalaskurit vastaavat odotettua.
//! - `top_order_correct` — 1.0 jos `top_memories` on laskevassa
//!   tärkeysjärjestyksessä eikä sisällä haudattuja.
//! - `conflicts_correct` — 1.0 jos ristiriitalaskuri vastaa odotettua.
//! - `retrievable_ratio` — haettavien (aktiivinen + arkistoitu) osuus kaikista.
//!
//! ## Reprodusoitavuus
//! `now` injektoidaan [`Scenario::run`]:n `clock`-parametrina — katsaus
//! ottaa hetken parametrina (ei järjestelmäkellosta) ja järjestää tuloksensa
//! vakaasti (tärkeys laskeva, tasapeli pienempi id), joten sama kylvö tuottaa
//! aina saman raportin (design §2.2).
//!
//! ## Subjektin rooli
//! [`Scenario::run`] saa subjektin mustana laatikkona ([`Subject`]); subjektin
//! elävyys varmistetaan kevyellä `sleep_cycle`-kutsulla joka ei saa kaataa
//! subjektia. Auktoritatiiviset mittarit lasketaan omistetusta kylvetystä
//! tallennuksesta, koska viikkokatsaus on memory/dream-tason invariantti joka
//! on sama kaikille subjekteille.

use async_trait::async_trait;

use familyclaw_core::Timestamp;
use familyclaw_dream::{tag_conflict, weekly_review, WeeklyReport};
use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore};

use crate::error::{BenchError, Result};
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// S8 Weekly Review -skenaario.
///
/// Tilaton arvo; kaikki ajotila kylvetään [`Scenario::run`]:ssa injektoidun
/// kellon suhteen, joten skenaarion voi ajaa monta kertaa ja saada saman
/// tuloksen.
#[derive(Debug, Default, Clone, Copy)]
pub struct WeeklyReviewScenario;

impl WeeklyReviewScenario {
    /// Skenaarion vakaa tunniste.
    pub const ID: &'static str = "s8_weekly_review";

    /// Rakentaa skenaarion.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Tunnettu kylvö: odotetut laskurit ja tärkeysjärjestys jälkikäteistä
/// arviointia varten.
struct Seeded {
    /// Muistojen kokonaismäärä.
    total: usize,
    /// Aktiivisten määrä.
    active: usize,
    /// Arkistoitujen määrä.
    archived: usize,
    /// Haudattujen määrä.
    tombstoned: usize,
    /// Haettavien (aktiivinen + arkistoitu) määrä = `consolidated`.
    consolidated: usize,
    /// Ristiriitatägättyjen muistojen määrä (tägätty pari → 2).
    conflicted: usize,
    /// Haettavien muistojen sisällöt tärkeys laskevassa järjestyksessä —
    /// odotettu `top_memories`-järjestys.
    expected_top_order: Vec<String>,
}

impl WeeklyReviewScenario {
    /// Kylvää tunnetun tallennuksen injektoidun kellon suhteen ja palauttaa
    /// odotetut laskurit + tärkeysjärjestyksen.
    ///
    /// Kylvö (tärkeysarvot valittu erottuviksi ettei tasapeli sotke
    /// järjestystä):
    /// - 3 aktiivista (tärkeydet 0.9, 0.5, 0.2),
    /// - 1 arkistoitu (tärkeys 0.7) — haettava, joten mukana top-listalla,
    /// - 1 haudattu (tärkeys 0.95) — EI saa nousta esiin,
    /// - 1 ristiriitapari (kaksi muistoa) tägätään → `conflicted == 2`.
    async fn seed(store: &LocalJsonStore, clock: Timestamp) -> Result<Seeded> {
        // — Aktiiviset muistot (haettavia, eri tärkeyksillä) ————————————————
        let high = add(store, mem("the launch shipped on time", 0.9, clock)).await?;
        add(store, mem("a mid-priority note about testing", 0.5, clock)).await?;
        add(store, mem("a low-priority passing thought", 0.2, clock)).await?;

        // — Arkistoitu (yhä haettava, top-listalle) ——————————————————————————
        let archived = add(
            store,
            mem("an older but still relevant decision", 0.7, clock),
        )
        .await?;
        store
            .set_status(archived, MemoryStatus::Archived)
            .await
            .map_err(BenchError::from)?;

        // — Haudattu (korkea tärkeys, mutta EI saa nousta top-listalle) ———————
        let tombstoned = add(store, mem("a retracted false claim", 0.95, clock)).await?;
        store
            .set_status(tombstoned, MemoryStatus::Tombstoned)
            .await
            .map_err(BenchError::from)?;

        // — Ristiriitapari (kaksi aktiivista) tägätään ————————————————————————
        let conflict_a = add(store, mem("agent_a is in region one", 0.4, clock)).await?;
        let conflict_b = add(store, mem("agent_a is in region two", 0.3, clock)).await?;
        tag_conflict(store, conflict_a, conflict_b, clock)
            .await
            .map_err(BenchError::from)?;

        // Kylvettyjä yhteensä: 3 aktiivista + 1 arkistoitu + 1 haudattu + 2
        // ristiriitaista (aktiivisia) = 7.
        // active = 3 + 2 (ristiriitaparin osapuolet) = 5.
        // archived = 1, tombstoned = 1.
        // consolidated (haettavat) = active + archived = 6.
        // conflicted = 2 (tägätyn parin molemmat).
        //
        // Haettavat tärkeys laskevassa järjestyksessä (haudattu jätetään pois):
        //   0.9 launch, 0.7 archived, 0.5 mid, 0.4 conflict_a,
        //   0.3 conflict_b, 0.2 low.
        // `weekly_review` rajaa top_memories DEFAULT_TOP_N (=5) ensimmäiseen,
        // joten odotettu järjestys on viisi tärkeintä.
        let _ = (high, conflict_a, conflict_b);
        let expected_top_order = vec![
            "the launch shipped on time".to_string(),
            "an older but still relevant decision".to_string(),
            "a mid-priority note about testing".to_string(),
            "agent_a is in region one".to_string(),
            "agent_a is in region two".to_string(),
        ];

        Ok(Seeded {
            total: 7,
            active: 5,
            archived: 1,
            tombstoned: 1,
            consolidated: 6,
            conflicted: 2,
            expected_top_order,
        })
    }
}

#[async_trait]
impl Scenario for WeeklyReviewScenario {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        // Elävyyskoe: subjektin oma unijakso ei saa kaatua. Tulos kirjataan
        // huomioksi; auktoritatiiviset mittarit lasketaan alla omistetusta
        // kylvöstä (katsaus on memory/dream-tason invariantti).
        let subject_summary = subject.sleep_cycle(clock).await?;

        // Kylvä tunnettu tallennus + tägää ristiriitapari injektoidun kellon
        // suhteen.
        let store = LocalJsonStore::in_memory();
        let seeded = Self::seed(&store, clock).await?;

        // Aja AITO viikkokatsaus injektoidulla hetkellä, sitten arvioi.
        let report = weekly_review(&store, clock)
            .await
            .map_err(BenchError::from)?;
        let outcome = Outcome::evaluate(&seeded, &report)?;

        let result = ScenarioResult::new(Self::ID, outcome.passed)
            .with_metric("counts_correct", bool_metric(outcome.counts_correct))
            .with_metric("top_order_correct", bool_metric(outcome.top_order_correct))
            .with_metric("conflicts_correct", bool_metric(outcome.conflicts_correct))
            .with_metric("retrievable_ratio", outcome.retrievable_ratio)
            .with_note(format!(
                "counts: total={} active={} archived={} tombstoned={} consolidated={} (expected total={})",
                report.total,
                report.active,
                report.archived,
                report.tombstoned,
                report.consolidated,
                seeded.total
            ))
            .with_note(format!(
                "top_memories: {} listed, order_correct={} (buried memory excluded)",
                report.top_memories.len(),
                outcome.top_order_correct
            ))
            .with_note(format!(
                "conflicted={} (expected {}), conflicts_listed={}",
                report.conflicted,
                seeded.conflicted,
                report.conflicts.len()
            ))
            .with_note(format!(
                "subject sleep_cycle liveness: scanned={} protected_core_intact={}",
                subject_summary.scanned, subject_summary.protected_core_intact
            ));

        Ok(result)
    }
}

/// Viikkokatsauksen arvio: kaikki S8-mittarit ja läpäisytulos yhdessä paikassa.
///
/// Liput ovat toisistaan riippumattomia diagnostisia tuloksia (laskurit /
/// järjestys / ristiriidat erikseen), eivät tilakoneen tiloja — siksi neljä
/// erillistä booleania on selkein esitys (ei `struct_excessive_bools`-uudelleen-
/// muotoilua kaksiarvoisiksi enumeiksi, joka vain hämärtäisi merkityksen).
#[allow(clippy::struct_excessive_bools)]
struct Outcome {
    /// Vastaavatko kaikki tilalaskurit kylvettyä.
    counts_correct: bool,
    /// Onko `top_memories` laskevassa tärkeysjärjestyksessä eikä sisällä
    /// haudattuja, ja vastaako se odotettua sisältöjärjestystä.
    top_order_correct: bool,
    /// Vastaako ristiriitalaskuri kylvettyä paria.
    conflicts_correct: bool,
    /// Haettavien osuus kaikista (`consolidated / total`).
    retrievable_ratio: f64,
    /// Koko skenaarion läpäisytulos.
    passed: bool,
}

impl Outcome {
    /// Arvioi viikkokatsauksen vertaamalla raporttia tunnettuun kylvöön.
    ///
    /// # Errors
    /// [`BenchError`] jos kylvö oli tyhjä (nollajako-suoja).
    fn evaluate(seeded: &Seeded, report: &WeeklyReport) -> Result<Self> {
        if seeded.total == 0 {
            return Err(BenchError::scenario(
                "weekly_review: seed produced an empty store",
            ));
        }

        let counts_correct = report.total == seeded.total
            && report.active == seeded.active
            && report.archived == seeded.archived
            && report.tombstoned == seeded.tombstoned
            && report.consolidated == seeded.consolidated;

        // Top-järjestys: sisällöt vastaavat odotettua laskevaa järjestystä JA
        // tärkeydet ovat ei-kasvavia (varmuuden vuoksi).
        let actual_top: Vec<&str> = report
            .top_memories
            .iter()
            .map(|d| d.content.as_str())
            .collect();
        let expected_top: Vec<&str> = seeded
            .expected_top_order
            .iter()
            .map(String::as_str)
            .collect();
        let order_matches = actual_top == expected_top;
        let importance_non_increasing = report
            .top_memories
            .windows(2)
            .all(|w| w[0].importance >= w[1].importance);
        let top_order_correct = order_matches && importance_non_increasing;

        let conflicts_correct =
            report.conflicted == seeded.conflicted && report.conflicts.len() == seeded.conflicted;

        #[allow(clippy::cast_precision_loss)]
        let retrievable_ratio = report.consolidated as f64 / report.total as f64;

        let passed = counts_correct && top_order_correct && conflicts_correct;

        Ok(Self {
            counts_correct,
            top_order_correct,
            conflicts_correct,
            retrievable_ratio,
            passed,
        })
    }
}

/// Lisää muiston tallennukseen, kääräisten ydincraten virheen benchin virheeksi.
async fn add(store: &LocalJsonStore, memory: Memory) -> Result<familyclaw_core::MessageId> {
    store.add(memory).await.map_err(BenchError::from)
}

/// Muisto annetulla sisällöllä ja tärkeydellä injektoituun kelloon ankkuroituna.
fn mem(content: &str, importance: f32, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// Boolean → mittariluku (`1.0`/`0.0`) determinististä scorecardia varten.
fn bool_metric(ok: bool) -> f64 {
    if ok {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub-toteutus palauttaa literaalin.
#[allow(clippy::float_cmp)] // Vakiot 0.0/1.0 ovat tarkkoja float-arvoja testeissä.
mod tests {
    use super::*;

    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    /// Kiinteä injektoitu referenssikello (2026-06-05 12:00 UTC).
    fn clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_780_660_800).expect("valid clock")
    }

    /// Minimaalinen subjekti-stub joka ei kaadu — skenaario laskee
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
    async fn weekly_review_passes_all_money_metrics() {
        let mut subject = StubSubject;
        let scenario = WeeklyReviewScenario::new();
        let result = scenario.run(&mut subject, clock()).await.expect("run");

        assert_eq!(result.id, WeeklyReviewScenario::ID);
        assert_eq!(result.metrics.get("counts_correct").copied(), Some(1.0));
        assert_eq!(result.metrics.get("top_order_correct").copied(), Some(1.0));
        assert_eq!(result.metrics.get("conflicts_correct").copied(), Some(1.0));
        // Haettavat = 6/7.
        let ratio = result
            .metrics
            .get("retrievable_ratio")
            .copied()
            .expect("ratio metric");
        assert!(
            (ratio - 6.0 / 7.0).abs() < 1e-9,
            "odotettiin 6/7 haettavaa, sai {ratio}"
        );

        assert!(result.passed, "S8 pitäisi läpäistä: {:?}", result.notes);
    }

    #[tokio::test]
    async fn weekly_review_is_deterministic() {
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let scenario = WeeklyReviewScenario::new();
        let r1 = scenario.run(&mut s1, clock()).await.expect("r1");
        let r2 = scenario.run(&mut s2, clock()).await.expect("r2");
        // Sama injektoitu hetki → identtiset mittarit + huomiot (§2.2).
        assert_eq!(r1.metrics, r2.metrics);
        assert_eq!(r1.notes, r2.notes);
        assert_eq!(r1.passed, r2.passed);
    }

    #[tokio::test]
    async fn buried_memory_is_excluded_from_top() {
        // Suora todiste: korkein tärkeys (0.95) on haudattu, joten se EI saa
        // olla top-listan kärjessä — kärjessä on 0.9 aktiivinen.
        let store = LocalJsonStore::in_memory();
        WeeklyReviewScenario::seed(&store, clock())
            .await
            .expect("seed");
        let report = weekly_review(&store, clock()).await.expect("review");
        assert_eq!(report.top_memories[0].content, "the launch shipped on time");
        assert!(report
            .top_memories
            .iter()
            .all(|d| d.content != "a retracted false claim"));
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(WeeklyReviewScenario::new().id(), "s8_weekly_review");
    }
}

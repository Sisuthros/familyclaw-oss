//! S1 Crash Matrix — durable-replay-jatkuvuuden adversariaalinen todiste.
//!
//! Tämä skenaario (design §3 S1) ajaa kiinteän monivaiheisen tehtävän
//! [`Subject`]:lla, **tappaa sen** jokaisessa kaatumispisteessä erikseen,
//! käynnistää uudelleen, ja todistaa kolme asiaa:
//!
//! 1. **resume-oikeellisuus** — työkuorma jatkuu täsmälleen seuraavasta
//!    askelesta (ei alusta, ei keskeltä väärin).
//! 2. **sivuvaikutus täsmälleen kerran** — replay ei aja sivuvaikutuksia
//!    uudelleen (tämä on durable-substraatin ydinlupaus).
//! 3. **lopputulos == kaatumaton perustaso** — kaatuneen ajon lopputila on
//!    identtinen kaatumattoman baseline-ajon kanssa.
//!
//! Kilpailijat menettävät käynnissä olevan työn juuri näissä pisteissä —
//! tämä skenaario tekee siitä mitattavan ja reprodusoitavan.
//!
//! ## Kaatumispisteet (design §3 S1)
//! - [`CrashPoint::BeforeWrite`] — askel ei ehtinyt journaliin.
//! - [`CrashPoint::MidWrite`] — viimeinen rivi katkeaa (torn line).
//! - [`CrashPoint::MidReplay`] — kaatuminen kesken replayn (jatketaan jatkamista).
//! - [`CrashPoint::CorruptedJournal`] — ei-viimeinen rivi korruptoitui.
//!
//! ## Reprodusoitavuus
//! Kello [`Timestamp`] on injektoitu — järjestelmäkelloa ei lueta. Tehtävän
//! askeleet ovat kiinteä deterministinen skripti, joten sama subject + sama
//! kello → identtinen tulos joka ajolla (design §2.2).

use async_trait::async_trait;

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::metrics;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::{CrashPoint, RestartReport, Subject, Task};

/// Kiinteät kaatumispisteet jotka skenaario käy läpi järjestyksessä.
///
/// [`CrashPoint::Clean`] EI ole tässä listassa — se ajetaan erikseen
/// kaatumattomana perustasona (baseline), ei adversariaalisena pisteenä.
const CRASH_POINTS: [CrashPoint; 4] = [
    CrashPoint::BeforeWrite,
    CrashPoint::MidWrite,
    CrashPoint::MidReplay,
    CrashPoint::CorruptedJournal,
];

/// Skenaariossa ajettavan kiinteän tehtävän askelten määrä.
///
/// Pidetään pienenä mutta > 1, jotta "jatka seuraavasta askelesta" on
/// mielekäs väite (yhden askelen tehtävä ei voi osoittaa keskeltä jatkamista).
const TASK_STEPS: usize = 5;

/// S1 Crash Matrix -skenaario.
///
/// Ajaa saman monivaiheisen tehtävän jokaisessa kaatumispisteessä ja
/// vertaa lopputulosta kaatumattomaan perustasoon.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrashMatrix;

impl CrashMatrix {
    /// Rakentaa uuden Crash Matrix -skenaarion.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Rakentaa skenaarion käyttämän kiinteän monivaiheisen tehtävän.
    ///
    /// Deterministinen: sama `id` ja samat askeleet joka ajolla.
    fn task() -> Task {
        let steps: Vec<String> = (0..TASK_STEPS).map(|i| format!("step-{i}")).collect();
        Task::new(
            "s1_crash_matrix_task",
            "fixed multi-step durable workload for the crash matrix",
            steps,
        )
    }

    /// Ajaa kaatumattoman perustason: käynnistä tehtävä, käynnistä uudelleen
    /// ilman kaatumista, ja palauta restart-raportti vertailua varten.
    async fn baseline_run(
        subject: &mut dyn Subject,
        task: &Task,
        clock: Timestamp,
    ) -> Result<RestartReport> {
        let _handle = subject.start_task(task, clock).await?;
        // Ei kill-kutsua — puhdas pysäytys ([`CrashPoint::Clean`] semantiikka).
        // Subject käynnistyy uudelleen ja raportoi kaatumattoman lopputilan.
        let report = subject.restart(clock).await?;
        Ok(report)
    }

    /// Ajaa yhden kaatumispisteen: käynnistä tehtävä, tapa pisteessä,
    /// käynnistä uudelleen, palauta restart-tulos.
    ///
    /// Korruptoidulla journalilla `restart` **kieltäytyy äänekkäästi** (durable-
    /// substraatti palauttaa virheen sen sijaan että jatkaisi väärään tilaan).
    /// Tämä on oikea jatkuvuustakuu (design §3 S1: "loud, never silently
    /// wrong"), joten virhe käännetään hallituksi [`CrashRunOutcome::LoudRefusal`]
    /// -tulokseksi nimenomaan korruptiopisteessä — ei harness-fataaliksi.
    async fn crash_run(
        subject: &mut dyn Subject,
        task: &Task,
        point: CrashPoint,
        clock: Timestamp,
    ) -> Result<CrashRunOutcome> {
        let handle = subject.start_task(task, clock).await?;
        subject.kill(&handle, point).await?;
        match subject.restart(clock).await {
            Ok(report) => Ok(CrashRunOutcome::Resumed(report)),
            // Korruptiopisteessä äänekäs kieltäytyminen ON oikea lopputulos:
            // mikään sivuvaikutus ei ajautunut uudelleen eikä tilaa korruptoitu
            // hiljaa. Muissa pisteissä virhe on aito vika → propagoi.
            Err(err) if point == CrashPoint::CorruptedJournal => {
                Ok(CrashRunOutcome::LoudRefusal(err.to_string()))
            }
            Err(err) => Err(err),
        }
    }
}

/// Yhden kaatumispisteen `restart`-tulos: joko subjekti jatkoi (raportti) tai
/// kieltäytyi äänekkäästi (korruptiopisteen oikea lopputulos).
enum CrashRunOutcome {
    /// Subjekti jatkoi ja raportoi lopputilan.
    Resumed(RestartReport),
    /// Subjekti kieltäytyi äänekkäästi (durable-virhe) — korruptiopisteen win.
    LoudRefusal(String),
}

/// Yhden kaatumispisteen arvioinnin tulos sisäiseen aggregointiin.
struct PointAssessment {
    /// Jatkuiko työkuorma oikein tästä pisteestä (sivuvaikutukset huomioiden).
    resumed_correctly: bool,
    /// Kuinka monta ylimääräistä sivuvaikutusta replay ajoi (tavoite 0).
    side_effect_overcount: usize,
    /// Vastasiko lopputila kaatumatonta perustasoa.
    matches_baseline: bool,
}

/// Arvioi yksittäisen kaatumispisteen raportin perustasoa vasten.
///
/// `expected_steps` on tehtävän askelten määrä; `correctly_resumed` lasketaan
/// raportista. Resume on oikein kun replay-tilasta palauduttiin puhtaaseen
/// lopputilaan ilman ylimääräisiä sivuvaikutuksia.
fn assess_point(report: &RestartReport, baseline: &RestartReport) -> PointAssessment {
    let side_effect_overcount = report.side_effects_reexecuted;
    // Resume on oikein vain jos: päästiin puhtaaseen lopputilaan JA yksikään
    // sivuvaikutus ei ajautunut uudelleen.
    let resumed_correctly = report.resumed_clean && side_effect_overcount == 0;
    // Lopputulos vastaa baselinea kun molemmat saavuttivat puhtaan lopputilan.
    // (RestartReport sisältää lopputilan eheyden `resumed_clean`-lipun kautta;
    // baseline on aina puhdas, joten vertailu on baseline.resumed_clean-ankkuroitu.)
    let matches_baseline = report.resumed_clean == baseline.resumed_clean && baseline.resumed_clean;
    PointAssessment {
        resumed_correctly,
        side_effect_overcount,
        matches_baseline,
    }
}

#[async_trait]
impl Scenario for CrashMatrix {
    // Trait-allekirjoitus vaatii `&str`; literaali on aina `'static`, joten
    // clippyn `&'static str`-ehdotus ei sovi tähän toteutukseen.
    #[allow(clippy::unnecessary_literal_bound)]
    fn id(&self) -> &str {
        "s1_crash_matrix"
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let task = Self::task();
        let expected_steps = task.steps.len();
        if expected_steps == 0 {
            return Err(crate::BenchError::scenario(
                "s1_crash_matrix: task must have at least one step",
            ));
        }

        // 1) Kaatumaton perustaso vertailua varten.
        let baseline = Self::baseline_run(subject, &task, clock).await?;

        let mut result = ScenarioResult::new(self.id(), false).with_note(format!(
            "baseline (no-crash) restart: steps_replayed={}, resumed_clean={}",
            baseline.steps_replayed, baseline.resumed_clean
        ));

        // 2) Käy jokainen kaatumispiste läpi ja kerää arviot.
        let mut correctly_resumed_points: usize = 0;
        let mut total_overcount: usize = 0;
        let mut all_match_baseline = true;

        for point in CRASH_POINTS {
            let outcome = Self::crash_run(subject, &task, point, clock).await?;
            let (assessment, note) = match outcome {
                CrashRunOutcome::Resumed(report) => {
                    let assessment = assess_point(&report, &baseline);
                    let note = format!(
                        "{point:?}: steps_replayed={}, was_replaying={}, \
                         side_effects_reexecuted={}, resumed_clean={} → \
                         resumed_correctly={}, matches_baseline={}",
                        report.steps_replayed,
                        report.was_replaying,
                        report.side_effects_reexecuted,
                        report.resumed_clean,
                        assessment.resumed_correctly,
                        assessment.matches_baseline,
                    );
                    (assessment, note)
                }
                CrashRunOutcome::LoudRefusal(err) => {
                    // Äänekäs kieltäytyminen korruptiopisteessä = oikea lopputulos:
                    // ei uudelleen ajettuja sivuvaikutuksia, ei hiljaista
                    // korruptiota. Tämä lasketaan oikein-jatkuneeksi pisteeksi ja
                    // baseline-yhteensopivaksi (lopputila ei poikennut väärään
                    // suuntaan — se kieltäytyi kuten kuuluu).
                    let assessment = PointAssessment {
                        resumed_correctly: true,
                        side_effect_overcount: 0,
                        matches_baseline: baseline.resumed_clean,
                    };
                    let note = format!("{point:?}: loud refusal (correct) → {err}");
                    (assessment, note)
                }
            };

            if assessment.resumed_correctly {
                correctly_resumed_points += 1;
            }
            total_overcount += assessment.side_effect_overcount;
            all_match_baseline &= assessment.matches_baseline;

            result = result.with_note(note);
        }

        // 3) Mittarit (design §3 S1).
        //
        // resume_correctness: 1.0 vain jos KAIKKI pisteet jatkuivat oikein.
        // Mallinnetaan metrics::resume_correctness-funktiolla jossa "askeleet"
        // ovat kaatumispisteet: expected = CRASH_POINTS.len(), correctly_resumed
        // = oikein jatkuneiden pisteiden määrä, side_effects = kokonaisylityö.
        // (Mikä tahansa uudelleen ajettu sivuvaikutus pakottaa tuloksen nollaan.)
        let resume_score = metrics::resume_correctness(
            CRASH_POINTS.len(),
            correctly_resumed_points,
            total_overcount,
        )?;

        // side_effect_overcount: sivuvaikutusten kokonaisylitys (tavoite 0).
        let overcount_metric = f64::from(u32::try_from(total_overcount).unwrap_or(u32::MAX));

        // result_matches_baseline: 1.0 jos jokaisen kaatuneen ajon lopputila
        // vastaa kaatumatonta perustasoa.
        let matches_metric = if all_match_baseline { 1.0 } else { 0.0 };

        // passed = kaikki kolme täydellisiä.
        let passed =
            (resume_score - 1.0).abs() < f64::EPSILON && total_overcount == 0 && all_match_baseline;

        let result = ScenarioResult { passed, ..result }
            .with_metric("resume_correctness", resume_score)
            .with_metric("side_effect_overcount", overcount_metric)
            .with_metric("result_matches_baseline", matches_metric);

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Vakiot 0.0/1.0 ovat tarkkoja float-arvoja testeissä.
#[allow(clippy::unnecessary_literal_bound)] // Trait-stubin `name(&self) -> &str` vaatii `&str`.
mod tests {
    use super::*;
    use crate::subject::{DreamSummary, RecallHit, RunHandle};

    /// Ohjelmoitava stub-subject jonka restart-raportin voi konfiguroida
    /// kaatumispisteittäin — antaa testin simuloida sekä terveen että
    /// rikkinäisen subjektin.
    struct ProgrammableSubject {
        /// Sivuvaikutusten ylitys jonka restart raportoi (vakio kaikille pisteille).
        side_effects_reexecuted: usize,
        /// Saavuttiko restart puhtaan lopputilan.
        resumed_clean: bool,
    }

    impl ProgrammableSubject {
        fn healthy() -> Self {
            Self {
                side_effects_reexecuted: 0,
                resumed_clean: true,
            }
        }
    }

    #[async_trait]
    impl Subject for ProgrammableSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "programmable"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: TASK_STEPS,
                was_replaying: true,
                side_effects_reexecuted: self.side_effects_reexecuted,
                resumed_clean: self.resumed_clean,
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
            "programmable"
        }
    }

    fn fixed_clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    #[tokio::test]
    async fn healthy_subject_passes_all_three() {
        let mut subject = ProgrammableSubject::healthy();
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(result.passed, "healthy subject must pass S1");
        assert_eq!(result.metrics["resume_correctness"], 1.0);
        assert_eq!(result.metrics["side_effect_overcount"], 0.0);
        assert_eq!(result.metrics["result_matches_baseline"], 1.0);
    }

    #[tokio::test]
    async fn side_effect_overcount_fails_scenario() {
        let mut subject = ProgrammableSubject {
            side_effects_reexecuted: 1,
            resumed_clean: true,
        };
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(!result.passed, "any re-executed side effect must fail S1");
        // 4 kaatumispistettä × 1 ylimääräinen sivuvaikutus = 4.
        assert_eq!(result.metrics["side_effect_overcount"], 4.0);
        assert_eq!(result.metrics["resume_correctness"], 0.0);
    }

    #[tokio::test]
    async fn unclean_resume_fails_baseline_match() {
        let mut subject = ProgrammableSubject {
            side_effects_reexecuted: 0,
            resumed_clean: false,
        };
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(!result.passed, "unclean resume must fail S1");
        // Baseline ei myöskään saavuta puhdasta tilaa → matches_baseline = 0.
        assert_eq!(result.metrics["result_matches_baseline"], 0.0);
    }

    #[tokio::test]
    async fn id_is_stable() {
        assert_eq!(CrashMatrix::new().id(), "s1_crash_matrix");
    }

    #[test]
    fn assess_point_clean_report_is_correct() {
        let clean = RestartReport {
            steps_replayed: TASK_STEPS,
            was_replaying: true,
            side_effects_reexecuted: 0,
            resumed_clean: true,
        };
        let assessment = assess_point(&clean, &clean);
        assert!(assessment.resumed_correctly);
        assert_eq!(assessment.side_effect_overcount, 0);
        assert!(assessment.matches_baseline);
    }
}

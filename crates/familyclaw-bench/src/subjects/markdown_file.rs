//! [`MarkdownFileSubject`] — REHELLINEN kilpailija-perustaso (`MEMORY.md`-malli).
//!
//! ## Tärkeä rehellisyysvaroitus
//! Tämä **EI ole** aito OpenClaw tai Hermes Agent. Se on *kilpailijan
//! muotoinen malli* (design §2.1): puhdas in-process-[`Subject`] joka jäljittää
//! **dokumentoidun** tiedosto-pohjaisen Markdown-muistin käyttäytymisen
//! (OpenClaw/Hermes-tyylinen `MEMORY.md`). Tarkoitus on saada jatkuvuus-
//! benchmarkista *vertailu kasvotusten* — ei väittää että tämä olisi jonkin
//! oikean tuotteen sisuskalut. Mallinnetut käyttäytymismallit ovat juuri ne
//! epäonnistumistilat jotka tekevät FamilyClawista paremman.
//!
//! ## Mallinnetut (dokumentoidut) kilpailijakäyttäytymiset
//! 1. **Muisti** — yksi in-memory `MEMORY.md`-tyylinen puskuri
//!    ([`Vec<String>`]) jolla on *bootstrap-budjetti* ([`BOOTSTRAP_BUDGET`]).
//!    Budjetin ylittyessä puskuri **katkaisee hiljaa vanhimman ensin**
//!    (OpenClawin dokumentoitu `MEMORY.md`-truncation). EI suojattua ydintä,
//!    EI decay-politiikkaa — tärkeät identiteettifaktat katkeavat kuten mikä
//!    tahansa muu rivi.
//! 2. **Restart** — EI deterministista crash-replayta. Uudelleenkäynnistyksessä
//!    se **ajaa tehtävän askeleet alusta uudelleen** (suorittaen sivuvaikutukset
//!    uudestaan). Saavuttaa samankaltaisen lopputilan mutta re-runilla, ei
//!    replaylla.
//! 3. **Recall** — naivi osajono-haku (mahdollisesti katkaistun) puskurin yli,
//!    relevanssi kiinteä `1.0` osumalle. Jos tärkeä fakta katkesi, recall
//!    palauttaa siitä tyhjän — tämä on retention-epäonnistuminen.
//! 4. **Sleep** — no-op-konsolidaatio: `protected_core_intact = false`, koska
//!    sillä EI ole suojattua ydintä (rehellinen mallinnus "ei eternal-threadia").
//!
//! ## Reprodusoitavuus
//! Sama tehtävä → samat luvut. Ei järjestelmäkelloa, ei satunnaisuutta — kaikki
//! ajan tarvitsevat operaatiot saavat [`Timestamp`]:n injektoituna eikä sitä
//! itse asiassa edes tarvita laskennassa (perustaso on puhtaasti
//! tilakoneellinen). Kello otetaan vastaan vain rajapinnan saumana.

use async_trait::async_trait;

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::subject::{
    CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task,
};

/// `MEMORY.md`-bootstrap-budjetti: montako riviä puskuriin mahtuu ennen kuin
/// vanhin katkaistaan hiljaa (OpenClawin dokumentoitu truncation-raja).
pub const BOOTSTRAP_BUDGET: usize = 8;

/// Nimetty kilpailijaprofiili tiedosto-pohjaiselle muistille.
///
/// Sama in-process-malli parametroituna kahden DOKUMENTOIDUN file-agentin
/// käyttäytymisen mukaan, jotta `compare` tuottaa **nimetyn** vertailun yhden
/// geneerisen baselinen sijaan. Edelleen rehellisesti *malli*, EI aito tuote
/// (ks. tiedoston rehellisyysvaroitus) — vain raja-arvot ovat profiilikohtaisia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompetitorProfile {
    /// Geneerinen `MEMORY.md` oldest-first -truncation (oletus, rivibudjetti).
    Generic,
    /// OpenClaw-tyylinen: dokumentoitu `MEMORY.md` bootstrap-budjetti, hiljainen
    /// oldest-first -truncation, ei suojattua ydintä.
    OpenClaw,
    /// Hermes-tyylinen: dokumentoitu kova merkkiraja (`MEMORY.md` ~2 200 merkkiä),
    /// joka katkaisee vanhimman kun summa ylittyy.
    Hermes,
}

impl CompetitorProfile {
    /// Profiilin vakaa subjektinimi scorecardiin.
    #[must_use]
    pub fn subject_name(self) -> &'static str {
        match self {
            Self::Generic => "markdown-file-baseline",
            Self::OpenClaw => "openclaw-memory-md-model",
            Self::Hermes => "hermes-memory-2k-model",
        }
    }

    /// Rivibudjetti (`None` = käytä merkkirajaa sen sijaan).
    #[must_use]
    fn line_budget(self) -> Option<usize> {
        match self {
            Self::Generic | Self::OpenClaw => Some(BOOTSTRAP_BUDGET),
            Self::Hermes => None,
        }
    }

    /// Kova merkkiraja (`None` = käytä rivibudjettia). Hermesin dokumentoitu
    /// `MEMORY.md`-katto on ~2 200 merkkiä.
    #[must_use]
    fn char_budget(self) -> Option<usize> {
        match self {
            Self::Hermes => Some(2_200),
            Self::Generic | Self::OpenClaw => None,
        }
    }
}

/// Rehellinen kilpailija-perustaso: tiedosto-pohjainen Markdown-muistimalli.
///
/// Puhdas in-process — ei lapsiprosessia. Pitää yhtä `MEMORY.md`-tyylistä
/// puskuria, seuraa aktiivista tehtävää sekä kaatumishetkeen mennessä
/// valmistuneiden askelten määrää (jotta restart osaa raportoida montako
/// sivuvaikutusta ajetaan uudelleen).
#[derive(Debug)]
pub struct MarkdownFileSubject {
    /// `MEMORY.md`-tyylinen muistipuskuri (vanhin ensin; katkaistaan päästä).
    buffer: Vec<String>,
    /// Aktiivinen tehtävä (asetettu [`start_task`](MarkdownFileSubject::start_task)issa).
    task: Option<Task>,
    /// Ennen kaatumista valmistuneiden askelten määrä (asetettu
    /// [`kill`](MarkdownFileSubject::kill)issa, kulutettu
    /// [`restart`](MarkdownFileSubject::restart)issa).
    completed_steps: usize,
    /// Oliko viimeisin kaatuminen puhdas (`Clean`) — määrää restartin tuloksen.
    last_crash_clean: bool,
    /// Subjektin vakaa nimi scorecardia varten.
    name: String,
    /// Nimetty kilpailijaprofiili (budjettiraja + nimi).
    profile: CompetitorProfile,
}

impl Default for MarkdownFileSubject {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownFileSubject {
    /// Rakentaa tuoreen perustason tyhjällä muistipuskurilla (geneerinen profiili).
    #[must_use]
    pub fn new() -> Self {
        Self::with_profile(CompetitorProfile::Generic)
    }

    /// Rakentaa perustason nimetyllä kilpailijaprofiililla (OpenClaw/Hermes/geneerinen).
    /// Budjettiraja ja subjektinimi tulevat profiilista; muu käytös on identtinen.
    #[must_use]
    pub fn with_profile(profile: CompetitorProfile) -> Self {
        Self {
            buffer: Vec::new(),
            task: None,
            completed_steps: 0,
            last_crash_clean: true,
            name: profile.subject_name().to_string(),
            profile,
        }
    }

    /// Lisää rivin `MEMORY.md`-puskuriin ja katkaisee hiljaa vanhimman ensin
    /// jos bootstrap-budjetti ylittyy.
    ///
    /// Tämä on rehellisen mallinnuksen ydin: EI suojattua ydintä eikä decay-
    /// politiikkaa — tärkein identiteettifakta katkeaa yhtä lailla kuin viimeisin
    /// triviaali rivi heti kun budjetti täyttyy.
    fn push_memory(&mut self, line: impl Into<String>) {
        self.buffer.push(line.into());
        // Profiilikohtainen oldest-first -truncation. Hiljainen — ei lokia,
        // ei suojausta: juuri tämä on dokumentoitu file-agent-epäonnistuminen.
        if let Some(max_lines) = self.profile.line_budget() {
            while self.buffer.len() > max_lines {
                self.buffer.remove(0);
            }
        }
        if let Some(max_chars) = self.profile.char_budget() {
            // Hermes-malli: kova merkkikatto (~2 200). Karsi vanhin kunnes
            // koko puskurin merkkisumma mahtuu. Pidä aina vähintään uusin rivi.
            while self.buffer.len() > 1
                && self.buffer.iter().map(String::len).sum::<usize>() > max_chars
            {
                self.buffer.remove(0);
            }
        }
    }

    /// Suorittaa tehtävän askeleet alusta loppuun kirjaten kunkin puskuriin
    /// (sivuvaikutuksen mallinnus). Palauttaa suoritettujen askelten määrän.
    ///
    /// Tätä kutsutaan sekä ensiajossa ([`kill`](MarkdownFileSubject::kill)) että
    /// uudelleenkäynnistyksessä ([`restart`](MarkdownFileSubject::restart)) —
    /// koska perustaso EI replayta vaan ajaa uudelleen.
    fn run_steps(&mut self, task: &Task) -> usize {
        for (idx, step) in task.steps.iter().enumerate() {
            self.push_memory(format!("[{}] step {}: {}", task.id, idx, step));
        }
        task.steps.len()
    }

    /// Aktiivinen tehtävä tai virhe jos
    /// [`start_task`](MarkdownFileSubject::start_task) puuttuu.
    fn require_task(&self) -> Result<Task> {
        self.task
            .clone()
            .ok_or_else(|| crate::BenchError::subject("no active task — call start_task first"))
    }

    /// Palauttaa nykyisen muistipuskurin (testejä ja introspektiota varten).
    #[must_use]
    pub fn buffer(&self) -> &[String] {
        &self.buffer
    }
}

#[async_trait]
impl Subject for MarkdownFileSubject {
    async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
        // Tuore tehtävä: nollaa kaatumistila. Muistipuskuria EI nollata —
        // `MEMORY.md` on pitkäikäinen ja kerää rivejä yli tehtävien (juuri siksi
        // se ylittää budjetin ja katkaisee identiteettifaktoja).
        self.task = Some(task.clone());
        self.completed_steps = 0;
        self.last_crash_clean = true;
        // Token = tehtävän id (läpinäkyvä viite; ei prosessia/journalia).
        Ok(RunHandle::new(task.id.clone(), task.id.clone()))
    }

    async fn kill(&mut self, _handle: &RunHandle, point: CrashPoint) -> Result<()> {
        let task = self.require_task()?;
        let total = task.steps.len();
        // Montako askelta oli VALMISTUNUT (sivuvaikutus ajettu) ennen kaatumista.
        // Deterministiset arvot per kaatumispiste — restart ajaa nämä uudelleen.
        let completed = match point {
            // Puhdas pysäytys tai kaatuminen kesken replayn: kaikki askeleet valmiina.
            CrashPoint::Clean | CrashPoint::MidReplay => total,
            // Kaatuminen ennen viimeisen askelen kirjoitusta: kaikki paitsi viimeinen valmiina.
            CrashPoint::BeforeWrite | CrashPoint::MidWrite | CrashPoint::CorruptedJournal => {
                total.saturating_sub(1)
            }
        };

        // Aja tehtävä kaatumiseen asti (sivuvaikutukset puskuriin). Clean ajaa
        // koko tehtävän; muut ajavat `completed` ensimmäistä askelta.
        for (idx, step) in task.steps.iter().take(completed).enumerate() {
            self.push_memory(format!("[{}] step {}: {}", task.id, idx, step));
        }

        self.completed_steps = completed;
        self.last_crash_clean = matches!(point, CrashPoint::Clean);
        Ok(())
    }

    async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
        let task = self.require_task()?;

        if self.last_crash_clean {
            // Puhdas perustaso: ei kaatumista → ei ajettavaa uudelleen.
            return Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            });
        }

        // Kaatumisen jälkeen: perustaso EI replayta journalista vaan AJAA
        // tehtävän askeleet alusta uudelleen → jo kertaalleen valmistuneet
        // sivuvaikutukset suoritetaan toistamiseen.
        let reexecuted = self.completed_steps;
        let _reached_end = self.run_steps(&task);

        Ok(RestartReport {
            // Ei replayta: ei lokista palautettuja askelia.
            steps_replayed: 0,
            // Ei koskaan replay-tilassa — re-run ei ole replay.
            was_replaying: false,
            // Jo valmistuneet askeleet ajetaan sivuvaikutuksineen uudelleen.
            side_effects_reexecuted: reexecuted,
            // Lopputila on samankaltainen mutta saavutettu re-runilla, ei
            // deterministisella replaylla — siksi EI puhdas jatko.
            resumed_clean: false,
        })
    }

    async fn recall(&mut self, query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
        // Naivi osajono-haku (mahdollisesti katkaistun) puskurin yli. Jos faktan
        // sisältävä rivi katkesi budjetin täyttyessä, osumaa ei tule — tämä on
        // retention-epäonnistuminen jota benchmark mittaa.
        let hits = self
            .buffer
            .iter()
            .filter(|line| line.contains(query))
            // Kiinteä relevanssi 1.0 osumalle — ei pisteytystä, ei lajittelua.
            .map(|line| RecallHit::new(line.clone(), 1.0))
            .collect();
        Ok(hits)
    }

    async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
        // No-op-konsolidaatio: perustaso ei deduplikoi, ei pudota ristiriitoja,
        // ei absolutisoi päiväyksiä, ei vahvista eikä arkistoi. Skannaa vain
        // puskurin koon. `protected_core_intact = false` on rehellinen totuus —
        // sillä EI ole suojattua ydintä (ei eternal-threadia).
        Ok(DreamSummary {
            scanned: self.buffer.len(),
            merged: 0,
            dropped: 0,
            dates_absolutized: 0,
            strengthened: 0,
            archived: 0,
            protected_core_intact: false,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;

    /// Kiinteä injektoitu kello — perustaso ei käytä sitä, mutta rajapinta vaatii.
    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid")
    }

    /// Tehtävä jossa `n` askelta (deterministinen skripti).
    fn task_with_steps(id: &str, n: usize) -> Task {
        let steps = (0..n).map(|i| format!("do-{i}")).collect();
        Task::new(id, "baseline demo", steps)
    }

    #[tokio::test]
    async fn clean_crash_restart_resumes_clean_with_no_reexecution() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-clean", 3);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        subject
            .kill(&handle, CrashPoint::Clean)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert!(report.resumed_clean, "clean crash → resumed_clean");
        assert_eq!(
            report.side_effects_reexecuted, 0,
            "clean crash → ei uudelleenajoa"
        );
        assert_eq!(report.steps_replayed, 0, "perustaso ei koskaan replayta");
        assert!(!report.was_replaying);
    }

    #[tokio::test]
    async fn crash_restart_reexecutes_side_effects_and_is_not_clean() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-crash", 4);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        // Kaatuminen ennen viimeisen askelen kirjoitusta: 3 askelta valmistui.
        subject
            .kill(&handle, CrashPoint::BeforeWrite)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert!(
            report.side_effects_reexecuted > 0,
            "kaatuminen → sivuvaikutuksia ajetaan uudelleen"
        );
        assert_eq!(
            report.side_effects_reexecuted, 3,
            "BeforeWrite jätti 3/4 valmiiksi → 3 uudelleenajoa"
        );
        assert!(!report.resumed_clean, "re-run ei ole puhdas jatko");
        assert_eq!(
            report.steps_replayed, 0,
            "perustaso ei replayta vaan re-runaa"
        );
        assert!(!report.was_replaying);
    }

    #[tokio::test]
    async fn mid_replay_crash_reexecutes_all_steps() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-midreplay", 5);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        subject
            .kill(&handle, CrashPoint::MidReplay)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert_eq!(
            report.side_effects_reexecuted, 5,
            "MidReplay → kaikki 5 askelta ajetaan uudelleen"
        );
        assert!(!report.resumed_clean);
    }

    #[tokio::test]
    async fn memory_truncates_oldest_first_and_recall_misses_truncated_fact() {
        let mut subject = MarkdownFileSubject::new();
        // Tärkeä identiteettifakta ENSIMMÄISENÄ — se on vanhin, joten se katkeaa
        // ensin kun budjetti ylittyy.
        let important = "IDENTITY: the maintainer is the family creator".to_string();
        subject.push_memory(important.clone());
        // Työnnä reilusti yli budjetin verran muuta tavaraa.
        for i in 0..(BOOTSTRAP_BUDGET + 5) {
            subject.push_memory(format!("trivia rivi {i}"));
        }

        // Puskuri ei ylitä budjettia (vanhimmat katkaistu).
        assert_eq!(
            subject.buffer().len(),
            BOOTSTRAP_BUDGET,
            "puskuri pysyy budjetin sisällä"
        );
        // Tärkein fakta katkesi (vanhin ensin).
        assert!(
            !subject.buffer().contains(&important),
            "tärkein fakta katkaistiin hiljaa"
        );

        // Recall ei löydä katkaistua faktaa → retention-epäonnistuminen.
        let hits = subject
            .recall("IDENTITY", fixed_clock())
            .await
            .expect("recall");
        assert!(
            hits.is_empty(),
            "katkaistua identiteettifaktaa ei voi enää muistaa"
        );

        // Mutta tuoreempi rivi löytyy yhä, relevanssilla 1.0.
        let hits = subject
            .recall("trivia rivi 5", fixed_clock())
            .await
            .expect("recall");
        assert_eq!(hits.len(), 1);
        assert!((hits[0].relevance - 1.0).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn sleep_cycle_has_no_protected_core() {
        let mut subject = MarkdownFileSubject::new();
        subject.push_memory("rivi a");
        subject.push_memory("rivi b");

        let summary = subject.sleep_cycle(fixed_clock()).await.expect("sleep");
        assert!(
            !summary.protected_core_intact,
            "perustasolla ei ole suojattua ydintä"
        );
        assert_eq!(summary.scanned, 2, "skannaa puskurin koon");
        assert_eq!(summary.merged, 0, "no-op konsolidaatio: ei dedupia");
        assert_eq!(summary.dropped, 0);
        assert_eq!(summary.dates_absolutized, 0);
        assert_eq!(summary.strengthened, 0);
        assert_eq!(summary.archived, 0);
    }

    #[tokio::test]
    async fn name_is_stable() {
        let subject = MarkdownFileSubject::new();
        assert_eq!(subject.name(), "markdown-file-baseline");
    }

    #[tokio::test]
    async fn named_profiles_have_stable_distinct_names() {
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::OpenClaw).name(),
            "openclaw-memory-md-model"
        );
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::Hermes).name(),
            "hermes-memory-2k-model"
        );
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::Generic).name(),
            "markdown-file-baseline"
        );
    }

    #[tokio::test]
    async fn openclaw_profile_truncates_by_line_budget_oldest_first() {
        let mut subject = MarkdownFileSubject::with_profile(CompetitorProfile::OpenClaw);
        let important = "IDENTITY: maintainer is the family creator".to_string();
        subject.push_memory(important.clone());
        for i in 0..(BOOTSTRAP_BUDGET + 5) {
            subject.push_memory(format!("trivia {i}"));
        }
        assert_eq!(subject.buffer().len(), BOOTSTRAP_BUDGET, "rivibudjetti");
        assert!(
            !subject.buffer().contains(&important),
            "OpenClaw-malli katkaisee identiteetin hiljaa (oldest-first)"
        );
    }

    #[tokio::test]
    async fn hermes_profile_truncates_by_char_budget() {
        let mut subject = MarkdownFileSubject::with_profile(CompetitorProfile::Hermes);
        // ~2 200 merkin katto: työnnä reilusti yli → vanhimmat karsiutuvat,
        // mutta rivimäärä EI ole rajattu (toisin kuin OpenClaw): char-katto hoitaa.
        let big = "x".repeat(500);
        for _ in 0..10 {
            subject.push_memory(big.clone());
        }
        let total: usize = subject.buffer().iter().map(String::len).sum();
        assert!(total <= 2_200, "Hermes-malli pitää merkkisumman katon alla");
        assert!(!subject.buffer().is_empty(), "uusin rivi säilyy aina");
    }

    #[tokio::test]
    async fn same_task_yields_same_numbers() {
        // Determinismi: kaksi identtistä ajoa → identtiset restart-luvut.
        async fn run() -> RestartReport {
            let mut subject = MarkdownFileSubject::new();
            let task = task_with_steps("t-det", 4);
            let handle = subject
                .start_task(&task, fixed_clock())
                .await
                .expect("start");
            subject
                .kill(&handle, CrashPoint::MidWrite)
                .await
                .expect("kill");
            subject.restart(fixed_clock()).await.expect("restart")
        }
        assert_eq!(run().await, run().await);
    }
}

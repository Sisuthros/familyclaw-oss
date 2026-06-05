//! [`FamilyClawSubject`] — ajaa `continuity_daemon`-binääriä mustana laatikkona.
//!
//! Tämä on ensimmäinen [`Subject`]-toteutus (design §2.1). Se EI kutsu
//! FamilyClaw-crateja suoraan vaan ajaa `continuity_daemon`-binääriä erillisinä
//! lapsiprosesseina — todistaen jatkuvuuden **aidon prosessirajan yli**
//! (sama malli kuin `familyclaw-agent/src/bin/crash_replay.rs`). Näin benchmark
//! mittaa mitä skeptikko itse voi ajaa, ei in-process-kirjastokutsua.
//!
//! ## Elinkaari
//! 1. [`start_task`](FamilyClawSubject::start_task) — varaa väliaikaisen
//!    journal- + store-polun ja tallentaa tehtävän (ei vielä aja daemonia).
//! 2. [`kill`](FamilyClawSubject::kill) — ajaa `continuity_daemon start
//!    --crash-at <point>` joka kirjoittaa tilan ja poistuu kaatumispisteessä
//!    (`Clean` ajaa loppuun).
//! 3. [`restart`](FamilyClawSubject::restart) — ajaa `resume`:n joka rakentaa
//!    kontekstin journalista, toistaa valmistuneet askeleet ja viimeistelee.
//! 4. [`recall`](FamilyClawSubject::recall) — ajaa `recall`:n persistoitua
//!    tallennusta vasten.
//! 5. [`sleep_cycle`](FamilyClawSubject::sleep_cycle) — ajaa `sleep`:n
//!    (yksi [`DreamCycle`](familyclaw_dream::DreamCycle)).
//!
//! ## Reprodusoitavuus
//! Kello injektoidaan jokaiseen daemon-kutsuun `--clock <rfc3339>`-argumenttina
//! ([`Timestamp`]) — daemon ei lue järjestelmäkelloa. Sama syöte → sama tulos.
//!
//! ## Binäärin paikannus
//! Testeissä käytetään `CARGO_BIN_EXE_continuity_daemon`-ympäristömuuttujaa
//! (Cargo asettaa sen). Muuten polun voi antaa eksplisiittisesti tai antaa
//! ympäristömuuttujan `CONTINUITY_DAEMON_BIN` kautta; viimeisenä fallbackina
//! oletetaan että `continuity_daemon` on `PATH`:ssa.

use std::path::PathBuf;
use std::process::Output;

use async_trait::async_trait;
use familyclaw_core::{time, Timestamp};
use serde::Deserialize;

use crate::error::{BenchError, Result};
use crate::subject::{
    CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task,
};

/// Ympäristömuuttuja jolla daemon-binäärin polun voi yliajaa.
const DAEMON_BIN_ENV: &str = "CONTINUITY_DAEMON_BIN";

/// Cargon testiaikana asettama ympäristömuuttuja binäärin polulle.
const CARGO_BIN_ENV: &str = "CARGO_BIN_EXE_continuity_daemon";

/// FamilyClaw-subjekti joka ajaa `continuity_daemon`-binääriä lapsiprosessina.
///
/// Pitää väliaikaiset journal- + store-polut ja tehtävän tilan kahden
/// daemon-kutsun välillä. Polut elävät [`tempdir`](FamilyClawSubject::tempdir):n
/// alla ja siivotaan kun subjekti pudotetaan.
#[derive(Debug)]
pub struct FamilyClawSubject {
    /// Daemon-binäärin polku.
    daemon: PathBuf,
    /// Väliaikaishakemisto johon journal + store kirjoitetaan.
    tempdir: PathBuf,
    /// Journal-tiedoston polku.
    journal: PathBuf,
    /// Muistitallennuksen polku.
    store: PathBuf,
    /// Aktiivinen tehtävä (asetettu [`start_task`](FamilyClawSubject::start_task)issa).
    task: Option<Task>,
    /// Subjektin vakaa nimi scorecardia varten.
    name: String,
}

/// Daemonin `resume`-tuloste (jäsennetään stdoutin RESULT-riviltä).
#[derive(Debug, Deserialize)]
struct ResumeOutput {
    steps_replayed: usize,
    was_replaying: bool,
    fresh_steps: usize,
    resumed_clean: bool,
}

/// Daemonin yksittäinen recall-osuma.
#[derive(Debug, Deserialize)]
struct RecallHitOutput {
    content: String,
    relevance: f32,
}

/// Daemonin `recall`-tuloste.
#[derive(Debug, Deserialize)]
struct RecallOutput {
    hits: Vec<RecallHitOutput>,
}

/// Daemonin `sleep`-tuloste.
#[derive(Debug, Deserialize)]
struct SleepOutput {
    scanned: usize,
    merged: usize,
    dropped: usize,
    dates_absolutized: usize,
    strengthened: usize,
    archived: usize,
    protected_core_intact: bool,
}

impl FamilyClawSubject {
    /// Rakentaa subjektin annetulla daemon-binäärin polulla ja
    /// väliaikaishakemistolla.
    ///
    /// Useimmiten kannattaa käyttää [`from_env`](FamilyClawSubject::from_env)
    /// joka paikantaa binäärin ympäristöstä automaattisesti.
    #[must_use]
    pub fn new(daemon: impl Into<PathBuf>, tempdir: impl Into<PathBuf>) -> Self {
        let tempdir = tempdir.into();
        let journal = tempdir.join("continuity.journal.jsonl");
        let store = tempdir.join("continuity.store.json");
        Self {
            daemon: daemon.into(),
            tempdir,
            journal,
            store,
            task: None,
            name: "familyclaw".to_string(),
        }
    }

    /// Rakentaa subjektin paikantaen daemon-binäärin ympäristöstä ja luoden
    /// uniikin väliaikaishakemiston.
    ///
    /// Binäärin paikannusjärjestys:
    /// 1. `CONTINUITY_DAEMON_BIN` (eksplisiittinen yliajo),
    /// 2. `CARGO_BIN_EXE_continuity_daemon` (Cargo-testit),
    /// 3. `continuity_daemon` (`PATH`-fallback).
    ///
    /// # Errors
    /// [`BenchError::Io`] jos väliaikaishakemistoa ei voi luoda.
    pub fn from_env() -> Result<Self> {
        let daemon = resolve_daemon_bin();
        let tempdir = make_tempdir()?;
        Ok(Self::new(daemon, tempdir))
    }

    /// Palauttaa käytetyn väliaikaishakemiston polun.
    #[must_use]
    pub fn tempdir(&self) -> &std::path::Path {
        &self.tempdir
    }

    /// Poistaa mahdolliset aiemmat journal- + store-tiedostot (tuore lähtötila).
    fn reset_state(&self) -> Result<()> {
        for p in [&self.journal, &self.store] {
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }

    /// Ajaa daemon-alikomennon ja palauttaa sen [`Output`]:n.
    async fn run_daemon(&self, args: &[String]) -> Result<Output> {
        let daemon = self.daemon.clone();
        let owned: Vec<String> = args.to_vec();
        // Synkroninen `std::process::Command` blocking-säikeessä, jotta async-
        // konteksti ei jumitu (sama malli kuin crash_replay full-ajossa).
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&daemon).args(&owned).output()
        })
        .await
        .map_err(|e| BenchError::subject(format!("daemon join failed: {e}")))??;
        Ok(output)
    }

    /// Jäsentää daemonin stdoutista `RESULT <json>` -rivin annetuksi tyypiksi.
    fn parse_result<T: for<'de> Deserialize<'de>>(output: &Output) -> Result<T> {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|l| l.strip_prefix("RESULT "))
            .ok_or_else(|| {
                let stderr = String::from_utf8_lossy(&output.stderr);
                BenchError::subject(format!(
                    "daemon produced no RESULT line (stderr: {})",
                    stderr.trim()
                ))
            })?;
        let parsed: T = serde_json::from_str(line)?;
        Ok(parsed)
    }

    /// Tehtävä tai virhe jos [`start_task`](FamilyClawSubject::start_task) puuttuu.
    fn require_task(&self) -> Result<&Task> {
        self.task
            .as_ref()
            .ok_or_else(|| BenchError::subject("no active task — call start_task first"))
    }

    /// Ajaa `start`-komennon annetulla kaatumispisteellä (`None` = clean).
    async fn spawn_start(&self, point: Option<CrashPoint>, clock: Timestamp) -> Result<Output> {
        let task = self.require_task()?;
        let steps = task.steps.len().max(1);
        let mut args = vec![
            "start".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--task".to_string(),
            task.id.clone(),
            "--steps".to_string(),
            steps.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        if let Some(point) = point {
            args.push("--crash-at".to_string());
            args.push(crash_point_arg(point).to_string());
        }
        self.run_daemon(&args).await
    }
}

#[async_trait]
impl Subject for FamilyClawSubject {
    async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
        // Tuore lähtötila joka tehtävälle (deterministisyys).
        self.reset_state()?;
        self.task = Some(task.clone());
        // Token = journal-polku (subjekti-spesifinen läpinäkyvä viite).
        Ok(RunHandle::new(task.id.clone(), path_arg(&self.journal)))
    }

    async fn kill(&mut self, _handle: &RunHandle, point: CrashPoint) -> Result<()> {
        let clock = time::now();
        match point {
            CrashPoint::Clean => {
                // Ei kaatumista: aja tehtävä loppuun puhtaasti.
                let out = self.spawn_start(None, clock).await?;
                if !out.status.success() {
                    return Err(BenchError::subject(format!(
                        "clean start failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
            }
            CrashPoint::MidReplay => {
                // MidReplay vaatii valmiin journalin: aja ensin puhdas start,
                // sitten kaada kesken replayn re-enteröimällä.
                let clean = self.spawn_start(None, clock).await?;
                if !clean.status.success() {
                    return Err(BenchError::subject(
                        "mid_replay setup (clean start) failed".to_string(),
                    ));
                }
                // Tämä poistuu nollasta poikkeavalla koodilla — odotettu.
                let _ = self.spawn_start(Some(CrashPoint::MidReplay), clock).await?;
            }
            // BeforeWrite / MidWrite / CorruptedJournal: daemon poistuu pisteessä.
            other => {
                let _ = self.spawn_start(Some(other), clock).await?;
                if other == CrashPoint::CorruptedJournal {
                    // CorruptedJournal: daemon ei tue erikseen — simuloidaan
                    // vioittamalla EI-viimeinen rivi journalissa, jos rivejä on.
                    corrupt_middle_line(&self.journal)?;
                }
            }
        }
        Ok(())
    }

    async fn restart(&mut self, clock: Timestamp) -> Result<RestartReport> {
        let task = self.require_task()?;
        let steps = task.steps.len().max(1);
        let args = vec![
            "resume".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--task".to_string(),
            task.id.clone(),
            "--steps".to_string(),
            steps.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "resume failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: ResumeOutput = Self::parse_result(&output)?;
        Ok(RestartReport {
            steps_replayed: parsed.steps_replayed,
            was_replaying: parsed.was_replaying,
            // Tuoreet askeleet ovat normaali resumea — EIVÄT toistuneita
            // sivuvaikutuksia. side_effects_reexecuted on aina 0 niin kauan kuin
            // resume on puhdas; epäpuhdas resume nostaa tämän.
            side_effects_reexecuted: if parsed.resumed_clean {
                0
            } else {
                parsed.fresh_steps
            },
            resumed_clean: parsed.resumed_clean,
        })
    }

    async fn recall(&mut self, query: &str, clock: Timestamp) -> Result<Vec<RecallHit>> {
        let args = vec![
            "recall".to_string(),
            "--store".to_string(),
            path_arg(&self.store),
            "--query".to_string(),
            query.to_string(),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "recall failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: RecallOutput = Self::parse_result(&output)?;
        Ok(parsed
            .hits
            .into_iter()
            .map(|h| RecallHit::new(h.content, h.relevance))
            .collect())
    }

    async fn sleep_cycle(&mut self, clock: Timestamp) -> Result<DreamSummary> {
        // Liveness-koe (design §3 S3): aja unijakso TUOREEN, ehjän tilan yli.
        // Harness ajaa skenaariot peräkkäin samalla subjektilla, joten aiempi
        // skenaario (esim. S1 CorruptedJournal) on voinut jättää korruptoidun
        // journalin. Resetoidaan ja kylvetään puhdas valmis ajo, jotta `sleep`
        // lukee aina kelvollisen journalin + tallennuksen — ei aiemman
        // skenaarion korruptiojäämää.
        if let Some(task) = self.task.clone() {
            self.reset_state()?;
            let clean = self.spawn_start(None, clock).await?;
            if !clean.status.success() {
                return Err(BenchError::subject(format!(
                    "sleep_cycle setup (clean start) failed: {}",
                    String::from_utf8_lossy(&clean.stderr).trim()
                )));
            }
            // Pidä `task` aktiivisena (reset ei pyyhi sitä, mutta varmistetaan).
            self.task = Some(task);
        }
        // Varmista että journal on olemassa (sleep lukee ristiriidat siitä).
        if !self.journal.exists() {
            std::fs::write(&self.journal, b"")?;
        }
        let args = vec![
            "sleep".to_string(),
            "--journal".to_string(),
            path_arg(&self.journal),
            "--store".to_string(),
            path_arg(&self.store),
            "--clock".to_string(),
            time::to_rfc3339(clock),
        ];
        let output = self.run_daemon(&args).await?;
        if !output.status.success() {
            return Err(BenchError::subject(format!(
                "sleep failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let parsed: SleepOutput = Self::parse_result(&output)?;
        Ok(DreamSummary {
            scanned: parsed.scanned,
            merged: parsed.merged,
            dropped: parsed.dropped,
            dates_absolutized: parsed.dates_absolutized,
            strengthened: parsed.strengthened,
            archived: parsed.archived,
            protected_core_intact: parsed.protected_core_intact,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for FamilyClawSubject {
    fn drop(&mut self) {
        // Siivoa väliaikaishakemisto. Virheet ohitetaan (parhaansa-mukaan).
        let _ = std::fs::remove_dir_all(&self.tempdir);
    }
}

/// Paikantaa daemon-binäärin ympäristöstä (env-yliajo → Cargo → PATH-fallback).
fn resolve_daemon_bin() -> PathBuf {
    if let Ok(explicit) = std::env::var(DAEMON_BIN_ENV) {
        return PathBuf::from(explicit);
    }
    if let Ok(cargo) = std::env::var(CARGO_BIN_ENV) {
        return PathBuf::from(cargo);
    }
    PathBuf::from("continuity_daemon")
}

/// Luo uniikin väliaikaishakemiston bench-ajoa varten.
fn make_tempdir() -> Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "familyclaw-bench-{}-{}",
        std::process::id(),
        uniq_suffix()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Tuottaa karkean uniikin loppuliitteen hakemistonimeen (ei kelloriippuvuutta
/// determinismin kannalta — vain hakemiston eristämiseen rinnakkaisissa ajoissa).
fn uniq_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:08x}")
}

/// Muuntaa polun komentoriviargumentiksi (UTF-8; ei-UTF-8 polut lossy).
fn path_arg(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// [`CrashPoint`] daemonin `--crash-at`-arvoksi (snake_case).
fn crash_point_arg(point: CrashPoint) -> &'static str {
    match point {
        CrashPoint::BeforeWrite => "before_write",
        CrashPoint::MidWrite => "mid_write",
        CrashPoint::MidReplay => "mid_replay",
        // CorruptedJournal/Clean eivät kuljeta daemonille suoraan — käsitellään
        // kutsupuolella. Palautetaan turvallinen oletus.
        CrashPoint::CorruptedJournal | CrashPoint::Clean => "clean",
    }
}

/// Vioittaa journalin EI-viimeisen rivin (CorruptedJournal-hyökkäys, design §5).
///
/// Tämä on aito korruptio (toisin kuin revitty viimeinen rivi): jos rivejä on
/// vähintään kaksi, ensimmäinen rivi korvataan roskalla. `replay_from` palauttaa
/// tästä [`CorruptEntry`](familyclaw_durable::DurableError::CorruptEntry):n —
/// jota resume käsittelee virheenä (ei hiljaista vääristymää).
fn corrupt_middle_line(journal: &std::path::Path) -> Result<()> {
    if !journal.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(journal)?;
    let mut lines: Vec<String> = contents.lines().map(ToString::to_string).collect();
    if lines.len() < 2 {
        return Ok(());
    }
    lines[0] = "{ this is a corrupted middle line".to_string();
    let mut rebuilt = lines.join("\n");
    rebuilt.push('\n');
    std::fs::write(journal, rebuilt)?;
    Ok(())
}

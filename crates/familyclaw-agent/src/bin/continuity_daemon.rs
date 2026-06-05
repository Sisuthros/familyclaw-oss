//! `continuity_daemon` — musta laatikko jota `familyclaw-bench` ajaa lapsiprosessina.
//!
//! Tämä binääri laajentaa todistetun cross-process `crash_replay`-mallin
//! ([`crash_replay`](crate)) benchmark-ajettavaksi mustaksi laatikoksi (design
//! §4): bench-harness käynnistää sen `start`-, `resume`-, `recall`- ja
//! `sleep`-alikomennoilla, ja `--crash-at <point>` pakottaa `start`:in
//! poistumaan tahallaan kaatumispisteessä (`before_write` / `mid_write` /
//! `mid_replay`) jotta jatkuvuus voidaan todistaa aidon prosessirajan yli.
//!
//! ## Reprodusoitavuus (design §2.2)
//! Seinäkello **injektoidaan** `--clock <iso8601>` -argumentilla — binääri ei
//! lue järjestelmäkelloa koskaan. Sama syöte (`--journal` + `--store` + `--task`
//! + `--clock`) tuottaa identtisen lopputilan joka ajolla.
//!
//! ## Alikomennot
//! - `start --journal P --store P --task ID --steps N [--crash-at POINT] --clock TS`
//!   — ajaa `N` durable-askelta tehtävälle `ID`, kirjaa jokaisesta muiston, ja
//!   joko valmistuu puhtaasti tai poistuu kaatumispisteessä.
//! - `resume --journal P --store P --task ID --steps N --clock TS` — rakentaa
//!   `DurableContext`:n samasta journalista, toistaa valmistuneet askeleet
//!   ajamatta sivuvaikutuksia uudelleen, ajaa loput tuoreena ja tulostaa
//!   [`ResumeOutput`]-JSON:n.
//! - `recall --store P --query Q [--limit K] --clock TS` — hakee persistoidusta
//!   tallennuksesta ja tulostaa [`RecallOutput`]-JSON:n.
//! - `sleep --journal P --store P --clock TS` — ajaa yhden [`DreamCycle`]:n
//!   persistoidun tallennuksen + journalin yli ja tulostaa
//!   [`SleepOutput`]-JSON:n.
//!
//! Kaikki onnistuneet komennot tulostavat **yhden rivin JSON:ia** stdoutiin
//! (`RESULT <json>`), jonka harness jäsentää. Diagnostiikka menee stderriin.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use familyclaw_core::{time, Timestamp};
use familyclaw_durable::{DurableContext, FileJournal, Journal};
use familyclaw_dream::DreamCycle;
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStore, RetrievalContext,
};
use serde::{Deserialize, Serialize};

/// JSON-tuloksen etuliite stdoutilla — harness lukee tämän jälkeisen rivin.
const RESULT_PREFIX: &str = "RESULT ";

/// `continuity_daemon` -komentorivirajapinta.
#[derive(Parser)]
#[command(
    name = "continuity_daemon",
    about = "FamilyClaw continuity black box — driven by familyclaw-bench"
)]
struct Cli {
    /// Ajettava alikomento.
    #[command(subcommand)]
    command: Command,
}

/// Daemonin alikomennot.
#[derive(Subcommand)]
enum Command {
    /// Käynnistä tehtävä: aja durable-askeleet, kirjaa muistot, mahd. kaadu.
    Start(StartArgs),
    /// Jatka: rakenna konteksti journalista, toista + viimeistele, raportoi.
    Resume(ResumeArgs),
    /// Hae persistoidusta tallennuksesta.
    Recall(RecallArgs),
    /// Aja yksi unijakso (muistikonsolidaatio).
    Sleep(SleepArgs),
}

/// Mihin kohtaan `start` poistuu tahallaan (kaatumisen simulointi).
///
/// `--crash-at` ottaa nämä. `Clean` (oletus ilman lippua) ajaa loppuun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum CrashAt {
    /// Poistu ENNEN ensimmäistä journal-kirjoitusta (mitään ei levyllä).
    BeforeWrite,
    /// Poistu KESKEN journal-kirjoituksen — jätä revitty (torn) viimeinen rivi.
    MidWrite,
    /// Poistu KESKEN replayn — vain osa askeleista ehti uudelleen.
    MidReplay,
    /// Puhdas valmistuminen — ei kaatumista.
    Clean,
}

/// `start`-alikomennon argumentit.
#[derive(Parser)]
struct StartArgs {
    /// Journal-tiedoston polku (append-only JSONL).
    #[arg(long)]
    journal: PathBuf,
    /// Muistitallennuksen polku (JSON).
    #[arg(long)]
    store: PathBuf,
    /// Tehtävän vakaa tunniste (deterministinen).
    #[arg(long)]
    task: String,
    /// Suoritettavien askelten määrä.
    #[arg(long, default_value_t = 3)]
    steps: usize,
    /// Pakotettu kaatumispiste (oletus: ei kaatumista).
    #[arg(long, value_enum, default_value_t = CrashAt::Clean)]
    crash_at: CrashAt,
    /// Injektoitu seinäkello (ISO 8601 / RFC 3339).
    #[arg(long)]
    clock: String,
}

/// `resume`-alikomennon argumentit.
#[derive(Parser)]
struct ResumeArgs {
    /// Journal-tiedoston polku.
    #[arg(long)]
    journal: PathBuf,
    /// Muistitallennuksen polku.
    #[arg(long)]
    store: PathBuf,
    /// Tehtävän tunniste (sama kuin `start`:ssa).
    #[arg(long)]
    task: String,
    /// Askelten kokonaismäärä (sama kuin `start`:ssa).
    #[arg(long, default_value_t = 3)]
    steps: usize,
    /// Injektoitu seinäkello.
    #[arg(long)]
    clock: String,
}

/// `recall`-alikomennon argumentit.
#[derive(Parser)]
struct RecallArgs {
    /// Muistitallennuksen polku.
    #[arg(long)]
    store: PathBuf,
    /// Hakukysely.
    #[arg(long)]
    query: String,
    /// Palautettavien osumien yläraja.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Injektoitu seinäkello.
    #[arg(long)]
    clock: String,
}

/// `sleep`-alikomennon argumentit.
#[derive(Parser)]
struct SleepArgs {
    /// Journal-tiedoston polku (ristiriitamerkintöjä varten).
    #[arg(long)]
    journal: PathBuf,
    /// Muistitallennuksen polku.
    #[arg(long)]
    store: PathBuf,
    /// Injektoitu seinäkello.
    #[arg(long)]
    clock: String,
}

/// `resume`-komennon JSON-tuloste jonka harness jäsentää.
#[derive(Debug, Serialize, Deserialize)]
struct ResumeOutput {
    /// Lokista toistettujen valmistuneiden askelten määrä.
    steps_replayed: usize,
    /// Oliko konteksti replay-tilassa heti restartin jälkeen.
    was_replaying: bool,
    /// Tuoreena (replayn jälkeen) ajettujen askelten määrä.
    fresh_steps: usize,
    /// Saavutettiinko sama lopputila kuin kaatumattomalla ajolla.
    resumed_clean: bool,
}

/// Yksittäinen recall-osuma JSON:ssa.
#[derive(Debug, Serialize, Deserialize)]
struct RecallHitOutput {
    /// Muiston sisältö.
    content: String,
    /// Relevanssipistemäärä.
    relevance: f32,
}

/// `recall`-komennon JSON-tuloste.
#[derive(Debug, Serialize, Deserialize)]
struct RecallOutput {
    /// Osumat relevanssijärjestyksessä.
    hits: Vec<RecallHitOutput>,
}

/// `sleep`-komennon JSON-tuloste — peilaa [`DreamReport`]:n harnessille.
#[derive(Debug, Serialize, Deserialize)]
struct SleepOutput {
    /// Skannattujen muistojen määrä.
    scanned: usize,
    /// Yhdistettyjen duplikaattien määrä.
    merged: usize,
    /// Pudotettujen ristiriitaisten määrä.
    dropped: usize,
    /// Absolutisoitujen päiväysten määrä.
    dates_absolutized: usize,
    /// Vahvistettujen muistojen määrä.
    strengthened: usize,
    /// Arkistoitujen muistojen määrä.
    archived: usize,
    /// Säilyivätkö suojatut identiteetti-ankkurit koskemattomina.
    protected_core_intact: bool,
}

/// Daemonin sisäinen virhetyyppi.
///
/// Kaikki epäonnistumiset kulkevat tämän kautta — tuotantopolulla ei käytetä
/// `unwrap()`/`expect()`/`panic!()`. `main` muuntaa tämän stderr-viestiksi +
/// nollasta poikkeavaksi exit-koodiksi.
#[derive(Debug, thiserror::Error)]
enum DaemonError {
    /// Ydinalustan virhe (config, IO, muisti).
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),
    /// Durable-substraatin virhe (journal, replay).
    #[error("durable error: {0}")]
    Durable(#[from] familyclaw_durable::DurableError),
    /// JSON-sarjallistus epäonnistui.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    /// Tiedosto-IO epäonnistui.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Daemonin vakiotulostyyppi.
type DaemonResult<T> = std::result::Result<T, DaemonError>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Diagnostiikka stderriin; stdout varataan RESULT-riville.
            let _ = writeln!(std::io::stderr(), "continuity_daemon error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Kytkee alikomennon oikeaan käsittelijään.
async fn run(cli: Cli) -> DaemonResult<()> {
    match cli.command {
        Command::Start(args) => run_start(args).await,
        Command::Resume(args) => run_resume(args).await,
        Command::Recall(args) => run_recall(args).await,
        Command::Sleep(args) => run_sleep(args).await,
    }
}

/// Jäsentää injektoidun kellon RFC 3339 -muodosta.
fn parse_clock(raw: &str) -> DaemonResult<Timestamp> {
    Ok(time::parse_rfc3339(raw)?)
}

/// Tehtävän askeleen vakaa nimi (deterministinen replay-avain).
fn step_name(task: &str, index: usize) -> String {
    format!("{task}-step-{index}")
}

/// Askeleen tuottama deterministinen tulos (sivuvaikutuksen "hyötykuorma").
///
/// Sama indeksi → sama arvo joka ajolla, joten replay palauttaa identtisen
/// tuloksen ajamatta suljinta uudelleen.
fn step_payload(task: &str, index: usize) -> String {
    format!("{task} completed step {index}")
}

/// Kirjoittaa RESULT-rivin stdoutiin.
fn emit<T: Serialize>(value: &T) -> DaemonResult<()> {
    let json = serde_json::to_string(value)?;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{RESULT_PREFIX}{json}")?;
    stdout.flush()?;
    Ok(())
}

/// Käsittelee `start`: ajaa askeleet ja joko valmistuu tai kaatuu pisteessä.
async fn run_start(args: StartArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;

    // BeforeWrite: poistu ennen kuin mitään kirjoitetaan journaliin.
    if args.crash_at == CrashAt::BeforeWrite {
        eprintln!("crash injected: before_write (nothing persisted)");
        std::process::exit(137); // SIGKILL-tyylinen exit-koodi
    }

    let store = Arc::new(LocalJsonStore::open(&args.store).await?);

    // MidReplay: journalissa on jo valmistuneita askelia (aiemmasta ajosta).
    // Re-enteröidään replay ja poistutaan KESKEN sen — todistaa että replayn
    // keskeyttävä kaatuminen on toivuttava (resume-the-resume).
    if args.crash_at == CrashAt::MidReplay {
        let logged = count_completed_steps(&args.journal)?;
        let journal = FileJournal::open(&args.journal)?;
        let mut ctx = DurableContext::new(journal)?;
        // Toista vain puolet lokitetuista askelista, sitten poistu.
        let replay_until = logged / 2;
        for index in 0..replay_until {
            let name = step_name(&args.task, index);
            let payload = step_payload(&args.task, index);
            let _: String = ctx.step(&name, move || Ok(payload))?;
        }
        eprintln!(
            "crash injected: mid_replay (exited after replaying {replay_until}/{logged} step(s))"
        );
        std::process::exit(137);
    }

    // MidWrite: kaada viimeisen askeleen kirjoituksen "keskelle".
    let crash_step = if args.crash_at == CrashAt::MidWrite {
        Some(args.steps.saturating_sub(1))
    } else {
        None
    };

    {
        let journal = FileJournal::open(&args.journal)?;
        let mut ctx = DurableContext::new(journal)?;

        for index in 0..args.steps {
            if Some(index) == crash_step {
                // MidWrite: kirjoita revitty viimeinen rivi ja poistu.
                // Ensin valmistuneet askeleet ovat jo levyllä (ehjiä rivejä);
                // lisätään aito torn-rivi suoraan tiedostoon.
                drop(ctx);
                write_torn_line(&args.journal, &step_name(&args.task, index))?;
                eprintln!("crash injected: mid_write (torn last line at step {index})");
                std::process::exit(137);
            }

            let name = step_name(&args.task, index);
            let payload = step_payload(&args.task, index);
            // Durable-askel: tuoreessa ajossa suljin ajetaan ja tulos kirjataan.
            let recorded: String = ctx.step(&name, move || Ok(payload))?;

            // Sivuvaikutus (muistikirjaus) ajetaan vain tuoreessa ajossa —
            // turn_key tekee siitä idempotentin replayn yli.
            persist_step_memory(&store, &args.task, index, &recorded, clock).await?;
        }
        // ctx droppautuu tässä; journal on jo flushattu joka askeleella.
    }

    eprintln!(
        "start complete: {} step(s) for task {}",
        args.steps, args.task
    );
    Ok(())
}

/// Kirjoittaa aidon revityn (torn) viimeisen rivin journal-tiedostoon.
///
/// Tämä tuottaa klassisen "kaatuminen kesken kirjoituksen" -tilan: rivinvaihdoton
/// vajaa JSON-objekti tiedoston loppuun. [`DurableContext::new`] ohittaa tämän
/// (rivi ei jäsenny `StepCompleted`:ksi), joten resume jatkaa oikealta askelelta.
fn write_torn_line(journal: &PathBuf, step: &str) -> DaemonResult<()> {
    use std::fs::OpenOptions;
    let mut f = OpenOptions::new().append(true).create(true).open(journal)?;
    // Vajaa rivi: alkaa kuin oikea entry mutta katkeaa kesken — EI rivinvaihtoa.
    // (EntryKind serde-tagi on "kind"=snake_case; rivi katkeaa ennen sulkua.)
    write!(
        f,
        "{{\"step_id\":999,\"timestamp\":\"2026\",\"kind\":\"step_completed\",\"name\":\"{step}\",\"out"
    )?;
    f.flush()?;
    Ok(())
}

/// Käsittelee `resume`: replay journalista + tuoreet askeleet, raportoi.
async fn run_resume(args: ResumeArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;
    let store = Arc::new(LocalJsonStore::open(&args.store).await?);

    let journal = FileJournal::open(&args.journal)?;
    let mut ctx = DurableContext::new(journal)?;
    let was_replaying = ctx.is_replaying();
    let replayed_before = ctx.steps_taken();
    // Replay-vektorin koko = montako askelta lokissa oli ennen tuoretta ajoa.
    let steps_in_log = count_completed_steps(&args.journal)?;

    let mut fresh_steps = 0usize;
    for index in 0..args.steps {
        let was_fresh = !ctx.is_replaying();
        let name = step_name(&args.task, index);
        let payload = step_payload(&args.task, index);
        let recorded: String = ctx.step(&name, move || Ok(payload))?;

        if was_fresh {
            fresh_steps += 1;
            // Idempotentti muistikirjaus tuoreille askeleille (turn_key suojaa).
            persist_step_memory(&store, &args.task, index, &recorded, clock).await?;
        }
    }

    // Lopputila on "puhdas" jos kaikki askeleet on nyt suoritettu ja
    // muistitallennuksessa on tasan `steps` muistoa tälle tehtävälle.
    let task_memories = count_task_memories(&store, &args.task).await?;
    let resumed_clean = ctx.steps_taken() == args.steps && task_memories == args.steps;

    let _ = replayed_before; // aina 0 (kursori alkaa nollasta)
    let output = ResumeOutput {
        steps_replayed: steps_in_log.min(args.steps),
        was_replaying,
        fresh_steps,
        resumed_clean,
    };
    emit(&output)
}

/// Käsittelee `recall`: hakee tallennuksesta ja tulostaa osumat.
async fn run_recall(args: RecallArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;
    let store = LocalJsonStore::open(&args.store).await?;

    let ctx = RetrievalContext::new(&args.query).with_limit(args.limit);
    let results = store.retrieve(&ctx, clock).await?;

    let hits = results
        .into_iter()
        .map(|r| RecallHitOutput {
            content: r.memory.content,
            relevance: r.relevance,
        })
        .collect();
    emit(&RecallOutput { hits })
}

/// Käsittelee `sleep`: ajaa yhden unijakson ja tulostaa tiivistelmän.
async fn run_sleep(args: SleepArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;
    let store = LocalJsonStore::open(&args.store).await?;

    // Suojattujen ankkureiden tila ennen unta (eheyden todistamiseksi).
    let anchors_before = count_protected_active(&store).await?;

    let journal = FileJournal::open(&args.journal)?;
    let cycle = DreamCycle::new(&store);
    let report = cycle.run(&journal, clock).await?;

    let anchors_after = count_protected_active(&store).await?;
    let protected_core_intact = anchors_after == anchors_before;

    let output = SleepOutput {
        scanned: report.scanned,
        merged: report.merged,
        dropped: report.dropped,
        dates_absolutized: report.dates_absolutized,
        strengthened: report.strengthened,
        archived: report.archived,
        protected_core_intact,
    };
    emit(&output)
}

/// Kirjaa yhden askeleen muiston tallennukseen idempotentisti (`turn_key`).
async fn persist_step_memory(
    store: &Arc<LocalJsonStore>,
    task: &str,
    index: usize,
    content: &str,
    clock: Timestamp,
) -> DaemonResult<()> {
    let mut memory = Memory::builder(content)
        .factors(ImportanceFactors::new(0.5, 0.0, 0.3, 0.0))
        .decay_policy(DecayPolicy::Normal)
        .created_at(clock)
        .source("continuity_daemon")
        .tags([format!("task:{task}")])
        .build();
    // Idempotenssi: sama tehtävä+askel → sama avain → ei duplikaattia replayssa.
    memory.turn_key = Some(format!("{task}:step-{index}"));
    store.add(memory).await?;
    Ok(())
}

/// Laskee tietylle tehtävälle kuuluvat (tag:llä merkityt) aktiiviset muistot.
async fn count_task_memories(store: &Arc<LocalJsonStore>, task: &str) -> DaemonResult<usize> {
    let tag = format!("task:{task}");
    let all = store.all().await?;
    Ok(all
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &tag))
        .count())
}

/// Laskee aktiiviset suojatun ytimen (`ProtectedCore`) muistot.
async fn count_protected_active(store: &LocalJsonStore) -> DaemonResult<usize> {
    use familyclaw_memory::MemoryStatus;
    let all = store.all().await?;
    Ok(all
        .into_iter()
        .filter(|m| m.decay_policy.is_protected() && m.status == MemoryStatus::Active)
        .count())
}

/// Laskee journalin `StepCompleted`-rivit (revityt vajaat rivit eivät jäsenny).
fn count_completed_steps(journal: &PathBuf) -> DaemonResult<usize> {
    if !journal.exists() {
        return Ok(0);
    }
    let j = FileJournal::open(journal)?;
    let entries = j.replay_all()?;
    Ok(entries.iter().filter(|e| e.kind.is_step()).count())
}

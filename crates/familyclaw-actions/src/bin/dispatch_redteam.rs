//! `dispatch_redteam` — musta laatikko exactly-once-lähetyksen todistamiseen.
//!
//! Tämä binääri ajetaan lapsiprosessina (kuten `continuity_daemon`), jotta
//! "SIGKILL kesken lähetyksen" voidaan todistaa **aidon prosessirajan yli**. Se
//! kohdistuu täsmälleen siihen bugiin jonka GPT-5.5 paljasti: ikkuna jossa
//! [`ActionRuntime`]:n sivuvaikutus (`submit_task`) on jo tapahtunut mutta
//! agenttikerroksen durable-journalointi EI vielä — kaatuminen siinä saa replayn
//! ajamaan sivuvaikutuksen uudelleen (kaksoislaukaisu).
//!
//! ## Sivuvaikutuksen laskuri (todiste)
//! Rekisteröity taito ([`CountingExecutor`]) **kasvattaa levyllä olevaa laskuria
//! joka kerta kun sen `execute` ajetaan oikeasti**. Testi lukee laskurin raakana
//! ja vaatii että se on tasan 1 — kaksoislaukaisu nostaisi sen 2:een.
//!
//! ## Moodit (`--mode`)
//! - `old` — käyttää [`ActionRuntime::submit_task_as`]:ia (bugiton ennen korjausta:
//!   EI outbox-suojaa) → todistaa että bugi ON olemassa (laskuri = 2).
//! - `new` — käyttää [`ActionRuntime::submit_task_idempotent`]:ia kaatumiskestävän
//!   outboxin kanssa → todistaa korjauksen (laskuri = 1, lopputulos identtinen).
//!
//! ## Vaiheet (`--phase`)
//! - `crash` — aja lähetys (sivuvaikutus tapahtuu), kirjaa lopputulos
//!   `--outcome-out`-tiedostoon, ja **poistu 137 ENNEN kuin agentti ehtisi
//!   journaloida dispatch-rivin**. Tämä on se ikkuna jossa GPT-5.5:n bugi elää.
//! - `resume` — toistaa täsmälleen sen mitä agentin tuore-ajo-haara tekee kun
//!   journal-riviä EI ole (koska kaatuminen esti sen): ajaa SAMAN lähetyksen
//!   samalla idempotenssi-avaimella uudelleen. `new`-moodissa outbox palauttaa
//!   committed-lopputuloksen ajamatta sivuvaikutusta; `old`-moodissa se ajetaan
//!   uudelleen (double-fire).
//!
//! ## Determinismi
//! Kello injektoidaan `--clock`:lla — järjestelmäkelloa ei lueta koskaan.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use familyclaw_actions::dispatch_outbox::JournalDispatchOutbox;
use familyclaw_actions::executor::{ActionExecutor, ActionRequest, ActionResult};
use familyclaw_actions::manifest::{default_input_schema, SkillManifest};
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::skills::Skill;
use familyclaw_actions::{ActionRuntime, SkillId, SubmitOutcome};
use familyclaw_core::{time, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Laskurin kasvatuksen taito.
///
/// Jokainen `execute` kasvattaa **levyllä** olevaa sivuvaikutuslaskuria —
/// tämä on se mittari joka paljastaa kaksoislaukaisun yli prosessirajan.
///
/// Taito on tarkoituksella **auto-run** ([`ActionRisk::ReadOnly`] +
/// [`ApprovalPolicy::AutoIfReadOnly`]), jotta `submit_task` AJAA suorittajan
/// (= sivuvaikutuksen) heti ensimmäisellä kutsulla — eikä jää odottamaan
/// hyväksyntää. Näin "ulkoinen sivuvaikutus" tapahtuu mitattavasti jokaisella
/// `submit_task`-suorituksella, ja kaksoislaukaisu näkyy laskurissa suoraan.
#[derive(Debug)]
struct CountingExecutor {
    /// Polku jossa sivuvaikutuslaskuri elää (luetaan + kirjoitetaan joka ajolla).
    counter_path: PathBuf,
    /// Prosessin sisäinen laskuri (diagnostiikka; varsinainen todiste on levyllä).
    in_process: AtomicU64,
}

impl CountingExecutor {
    /// Kiinteä tunniste, jotta `start` ja `resume` viittaavat samaan taitoon.
    const SKILL_UUID: Uuid = uuid::uuid!("11111111-2222-4333-8444-555566667777");

    fn skill_id() -> SkillId {
        SkillId::from_uuid(Self::SKILL_UUID)
    }

    fn new(counter_path: PathBuf) -> Self {
        Self {
            counter_path,
            in_process: AtomicU64::new(0),
        }
    }

    /// Kasvattaa levyllä olevaa sivuvaikutuslaskuria atomisesti (luku → +1 → kirjoitus).
    fn bump_disk_counter(&self) {
        let current = std::fs::read_to_string(&self.counter_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = std::fs::write(&self.counter_path, (current + 1).to_string());
    }
}

#[async_trait]
impl ActionExecutor for CountingExecutor {
    async fn execute(&self, request: ActionRequest) -> familyclaw_actions::Result<ActionResult> {
        // SIVUVAIKUTUS: kasvata laskuria. Tämä on "ulkoinen vaikutus" jonka on
        // tapahduttava tasan kerran SIGKILL:n yli.
        self.in_process.fetch_add(1, Ordering::SeqCst);
        self.bump_disk_counter();
        Ok(ActionResult::success(
            "counter bumped",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl Skill for CountingExecutor {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (auto-run, suoritetaan heti)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: None,
            output_hint: None,
            input_schema: default_input_schema(),
        }
    }
}

/// Moodi: vanha (bugi) vai uusi (korjattu) lähetyspolku.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Mode {
    /// `submit_task_as` — EI outbox-idempotenssia (bugi ennen korjausta).
    Old,
    /// `submit_task_idempotent` — outbox-suojattu (korjaus).
    New,
}

/// Vaihe: kaadu kesken vai jatka.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Phase {
    /// Aja lähetys, kirjaa lopputulos, poistu 137 ENNEN journalointia.
    Crash,
    /// Aja SAMA lähetys uudelleen (kuten agentin tuore-haara ilman journal-riviä).
    Resume,
}

/// Komentorivirajapinta.
#[derive(Parser)]
#[command(name = "dispatch_redteam", about = "FamilyClaw exactly-once dispatch black box")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Ainoa alikomento: `run` (vaiheet erotellaan `--phase`:lla).
#[derive(Subcommand)]
enum Command {
    /// Aja yksi vaihe annetussa moodissa.
    Run(RunArgs),
}

/// `run`-argumentit.
#[derive(Parser)]
struct RunArgs {
    /// Vanha (bugi) vai uusi (korjattu) polku.
    #[arg(long, value_enum)]
    mode: Mode,
    /// Vaihe (`crash` / `resume`).
    #[arg(long, value_enum)]
    phase: Phase,
    /// Outbox-journalin polku (kaatumiskestävä idempotenssi).
    #[arg(long)]
    outbox: PathBuf,
    /// Sivuvaikutuslaskurin polku (todiste).
    #[arg(long)]
    counter: PathBuf,
    /// Tiedosto johon `crash`-vaihe kirjaa lopputuloksen (arvo-identtisyyden todiste).
    #[arg(long)]
    outcome_out: PathBuf,
    /// Stabiili idempotenssi-avain (= agentin `turn-{turn}-dispatch-{k}`).
    #[arg(long, default_value = "turn-0-dispatch-0")]
    key: String,
    /// Injektoitu seinäkello (RFC 3339).
    #[arg(long)]
    clock: String,
}

/// Lopputuloksen levymuoto arvo-identtisyyden vertailuun.
#[derive(Debug, Serialize, Deserialize)]
struct OutcomeRecord {
    task_id: String,
    pending_approval: Option<String>,
    status: String,
}

impl OutcomeRecord {
    fn from_submit(outcome: &SubmitOutcome) -> Self {
        Self {
            task_id: outcome.task_id.to_string(),
            pending_approval: outcome.pending_approval.map(|a| a.to_string()),
            status: format!("{:?}", outcome.status),
        }
    }
}

/// Daemonin virhetyyppi.
#[derive(Debug, thiserror::Error)]
enum HarnessError {
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),
    #[error("action error: {0}")]
    Action(#[from] familyclaw_actions::ActionError),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type HarnessResult<T> = std::result::Result<T, HarnessError>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "dispatch_redteam error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> HarnessResult<()> {
    match cli.command {
        Command::Run(args) => run_phase(args).await,
    }
}

/// Rakentaa ajoympäristön: laskuri-taito rekisteröitynä + (uusi-moodissa)
/// kaatumiskestävä outbox annetusta polusta.
fn build_runtime(args: &RunArgs) -> HarnessResult<ActionRuntime> {
    let mut runtime = ActionRuntime::new();
    runtime.register_skill(CountingExecutor::new(args.counter.clone()))?;
    if args.mode == Mode::New {
        let outbox = JournalDispatchOutbox::open(&args.outbox)?;
        runtime = runtime.with_dispatch_outbox(Box::new(outbox));
    }
    Ok(runtime)
}

/// Ajaa lähetyksen valitulla polulla (vanha vs uusi).
async fn dispatch(
    runtime: &mut ActionRuntime,
    args: &RunArgs,
    now: Timestamp,
) -> familyclaw_actions::Result<SubmitOutcome> {
    let payload = serde_json::json!({ "n": 1 });
    match args.mode {
        // VANHA polku: suora `submit_task_as` ilman idempotenssi-avainta. Tämä on
        // koodi joka oli ennen korjausta — sillä EI ole outbox-suojaa, joten
        // re-drive kaatumisen jälkeen ajaa sivuvaikutuksen uudelleen.
        Mode::Old => {
            runtime
                .submit_task_as("agent_a", CountingExecutor::skill_id(), payload, now)
                .await
        }
        // UUSI polku: idempotentti lähetys vakaalla avaimella. Outbox palauttaa
        // committed-lopputuloksen ajamatta sivuvaikutusta uudelleen.
        Mode::New => {
            runtime
                .submit_task_idempotent(
                    &args.key,
                    "agent_a",
                    CountingExecutor::skill_id(),
                    payload,
                    now,
                )
                .await
        }
    }
}

async fn run_phase(args: RunArgs) -> HarnessResult<()> {
    let now = time::parse_rfc3339(&args.clock)?;
    let mut runtime = build_runtime(&args)?;

    let outcome = dispatch(&mut runtime, &args, now).await?;

    match args.phase {
        Phase::Crash => {
            // Kirjaa lopputulos (arvo-identtisyyden vertailuun) ja poistu 137
            // ENNEN kuin agentti ehtisi journaloida dispatch-rivin. TÄSSÄ on
            // GPT-5.5:n ikkuna: sivuvaikutus on jo tapahtunut, journal-riviä ei ole.
            write_outcome(&args.outcome_out, &outcome)?;
            eprintln!("crash injected: after side effect, before dispatch journal append");
            std::process::exit(137);
        }
        Phase::Resume => {
            // Tämä on agentin tuore-haaran uudelleenajo (journal-riviä ei ole,
            // koska kaatuminen esti sen). `new`-moodissa outbox neutraloi sen;
            // `old`-moodissa sivuvaikutus tapahtuu toistamiseen.
            //
            // Vertaa lopputulosta `crash`-vaiheen kirjaamaan: arvo-identtisyys.
            let before = read_outcome(&args.outcome_out);
            let now_record = OutcomeRecord::from_submit(&outcome);
            let value_identical = before
                .as_ref()
                .is_some_and(|b| b.task_id == now_record.task_id);
            // Tulosta yhden rivin RESULT-JSON harnessille.
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "value_identical": value_identical,
                "resumed_task_id": now_record.task_id,
                "crashed_task_id": before.map(|b| b.task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
    }
}

/// Kirjoittaa lopputuloksen levylle (arvo-identtisyyden todiste).
fn write_outcome(path: &Path, outcome: &SubmitOutcome) -> HarnessResult<()> {
    let record = OutcomeRecord::from_submit(outcome);
    std::fs::write(path, serde_json::to_string(&record)?)?;
    Ok(())
}

/// Lukee aiemmin kirjatun lopputuloksen (jos on).
fn read_outcome(path: &Path) -> Option<OutcomeRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Lukee sivuvaikutuslaskurin raakana (0 jos tiedostoa ei ole).
fn read_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

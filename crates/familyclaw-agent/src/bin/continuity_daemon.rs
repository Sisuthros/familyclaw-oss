//! `continuity_daemon` — a black box run by `familyclaw-bench` as a child
//! process.
//!
//! This binary extends the proven cross-process `crash_replay` pattern
//! ([`crash_replay`](crate)) into a benchmark-runnable black box (design
//! §4): the bench harness launches it with `start`, `resume`, `recall`,
//! and `sleep` subcommands, and `--crash-at <point>` forces `start` to
//! exit deliberately at a crash point (`before_write` / `mid_write` /
//! `mid_replay`) so continuity can be proven across a genuine process
//! boundary.
//!
//! ## Reproducibility (design §2.2)
//! The wall clock is **injected** via the `--clock <iso8601>` argument —
//! the binary never reads the system clock. The same input (`--journal` +
//! `--store` + `--task` + `--clock`) produces an identical end state on
//! every run.
//!
//! ## Subcommands
//! - `start --journal P --store P --task ID --steps N [--crash-at POINT] --clock TS`
//!   — runs `N` durable steps for task `ID`, records a memory for each,
//!   and either finishes cleanly or exits at the crash point.
//! - `resume --journal P --store P --task ID --steps N --clock TS` —
//!   builds a `DurableContext` from the same journal, replays completed
//!   steps without re-executing side effects, runs the rest fresh, and
//!   prints [`ResumeOutput`] JSON.
//! - `recall --store P --query Q [--limit K] --clock TS` — queries the
//!   persisted store and prints [`RecallOutput`] JSON.
//! - `sleep --journal P --store P --clock TS` — runs one [`DreamCycle`]
//!   over the persisted store + journal and prints [`SleepOutput`] JSON.
//!
//! All successful commands print **one line of JSON** to stdout
//! (`RESULT <json>`), which the harness parses. Diagnostics go to stderr.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use familyclaw_core::{time, Timestamp};
use familyclaw_dream::DreamCycle;
use familyclaw_durable::{DurableContext, FileJournal, Journal};
use familyclaw_memory::{
    DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStore, RetrievalContext,
};
use serde::{Deserialize, Serialize};

/// JSON result prefix on stdout — the harness reads the line after this.
const RESULT_PREFIX: &str = "RESULT ";

/// `continuity_daemon` command-line interface.
#[derive(Parser)]
#[command(
    name = "continuity_daemon",
    about = "FamilyClaw continuity black box — driven by familyclaw-bench"
)]
struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// The daemon's subcommands.
#[derive(Subcommand)]
enum Command {
    /// Start a task: run durable steps, record memories, possibly crash.
    Start(StartArgs),
    /// Resume: build context from the journal, replay + finish, report.
    Resume(ResumeArgs),
    /// Query the persisted store.
    Recall(RecallArgs),
    /// Run a single sleep cycle (memory consolidation).
    Sleep(SleepArgs),
}

/// The point at which `start` deliberately exits (crash simulation).
///
/// `--crash-at` takes these. `Clean` (the default without the flag) runs
/// to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum CrashAt {
    /// Exit BEFORE the first journal write (nothing on disk).
    BeforeWrite,
    /// Exit DURING a journal write — leave a torn last line.
    MidWrite,
    /// Exit DURING replay — only some of the steps got replayed.
    MidReplay,
    /// Clean completion — no crash.
    Clean,
}

/// Arguments for the `start` subcommand.
#[derive(Parser)]
struct StartArgs {
    /// Path to the journal file (append-only JSONL).
    #[arg(long)]
    journal: PathBuf,
    /// Path to the memory store (JSON).
    #[arg(long)]
    store: PathBuf,
    /// The task's stable identifier (deterministic).
    #[arg(long)]
    task: String,
    /// Number of steps to run.
    #[arg(long, default_value_t = 3)]
    steps: usize,
    /// Forced crash point (default: no crash).
    #[arg(long, value_enum, default_value_t = CrashAt::Clean)]
    crash_at: CrashAt,
    /// Injected wall clock (ISO 8601 / RFC 3339).
    #[arg(long)]
    clock: String,
}

/// Arguments for the `resume` subcommand.
#[derive(Parser)]
struct ResumeArgs {
    /// Path to the journal file.
    #[arg(long)]
    journal: PathBuf,
    /// Path to the memory store.
    #[arg(long)]
    store: PathBuf,
    /// The task's identifier (same as in `start`).
    #[arg(long)]
    task: String,
    /// Total number of steps (same as in `start`).
    #[arg(long, default_value_t = 3)]
    steps: usize,
    /// Injected wall clock.
    #[arg(long)]
    clock: String,
}

/// Arguments for the `recall` subcommand.
#[derive(Parser)]
struct RecallArgs {
    /// Path to the memory store.
    #[arg(long)]
    store: PathBuf,
    /// The search query.
    #[arg(long)]
    query: String,
    /// Upper bound on the number of hits returned.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Injected wall clock.
    #[arg(long)]
    clock: String,
}

/// Arguments for the `sleep` subcommand.
#[derive(Parser)]
struct SleepArgs {
    /// Path to the journal file (for conflicting entries).
    #[arg(long)]
    journal: PathBuf,
    /// Path to the memory store.
    #[arg(long)]
    store: PathBuf,
    /// Injected wall clock.
    #[arg(long)]
    clock: String,
}

/// JSON output of the `resume` command, parsed by the harness.
#[derive(Debug, Serialize, Deserialize)]
struct ResumeOutput {
    /// Number of completed steps replayed from the log.
    steps_replayed: usize,
    /// Whether the context was in replay mode right after restart.
    was_replaying: bool,
    /// Number of steps run fresh (after replay).
    fresh_steps: usize,
    /// Whether the same end state was reached as in a crash-free run.
    resumed_clean: bool,
}

/// A single recall hit in the JSON output.
#[derive(Debug, Serialize, Deserialize)]
struct RecallHitOutput {
    /// The memory's content.
    content: String,
    /// Relevance score.
    relevance: f32,
}

/// JSON output of the `recall` command.
#[derive(Debug, Serialize, Deserialize)]
struct RecallOutput {
    /// Hits in relevance order.
    hits: Vec<RecallHitOutput>,
}

/// JSON output of the `sleep` command — mirrors
/// [`DreamReport`](familyclaw_dream::DreamReport) for the harness.
#[derive(Debug, Serialize, Deserialize)]
struct SleepOutput {
    /// Number of memories scanned.
    scanned: usize,
    /// Number of duplicates merged.
    merged: usize,
    /// Number of conflicting memories dropped.
    dropped: usize,
    /// Number of dates absolutized.
    dates_absolutized: usize,
    /// Number of memories strengthened.
    strengthened: usize,
    /// Number of memories archived.
    archived: usize,
    /// Whether protected identity anchors remained intact.
    protected_core_intact: bool,
}

/// The daemon's internal error type.
///
/// All failures flow through this — the production path never uses
/// `unwrap()`/`expect()`/`panic!()`. `main` converts this into a stderr
/// message + a non-zero exit code.
#[derive(Debug, thiserror::Error)]
enum DaemonError {
    /// Core platform error (config, IO, memory).
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),
    /// Durable substrate error (journal, replay).
    #[error("durable error: {0}")]
    Durable(#[from] familyclaw_durable::DurableError),
    /// JSON serialization failed.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    /// File IO failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// The daemon's standard result type.
type DaemonResult<T> = std::result::Result<T, DaemonError>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Diagnostics to stderr; stdout is reserved for the RESULT line.
            let _ = writeln!(std::io::stderr(), "continuity_daemon error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches the subcommand to its handler.
async fn run(cli: Cli) -> DaemonResult<()> {
    match cli.command {
        Command::Start(args) => run_start(args).await,
        Command::Resume(args) => run_resume(args).await,
        Command::Recall(args) => run_recall(args).await,
        Command::Sleep(args) => run_sleep(args).await,
    }
}

/// Parses the injected clock from RFC 3339 format.
fn parse_clock(raw: &str) -> DaemonResult<Timestamp> {
    Ok(time::parse_rfc3339(raw)?)
}

/// The task step's stable name (a deterministic replay key).
fn step_name(task: &str, index: usize) -> String {
    format!("{task}-step-{index}")
}

/// The deterministic result produced by a step (the side effect's
/// "payload").
///
/// Same index → same value on every run, so replay returns an identical
/// result without re-running the closure.
fn step_payload(task: &str, index: usize) -> String {
    format!("{task} completed step {index}")
}

/// Writes the RESULT line to stdout.
fn emit<T: Serialize>(value: &T) -> DaemonResult<()> {
    let json = serde_json::to_string(value)?;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{RESULT_PREFIX}{json}")?;
    stdout.flush()?;
    Ok(())
}

/// Handles `start`: runs the steps and either finishes or crashes at the
/// point.
async fn run_start(args: StartArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;

    // BeforeWrite: exit before anything is written to the journal.
    if args.crash_at == CrashAt::BeforeWrite {
        eprintln!("crash injected: before_write (nothing persisted)");
        std::process::exit(137); // SIGKILL-style exit code
    }

    let store = Arc::new(LocalJsonStore::open(&args.store).await?);

    // MidReplay: the journal already has completed steps (from an earlier
    // run). Re-enter replay and exit MID-replay — this proves that a
    // crash interrupting replay is itself recoverable (resume-the-resume).
    if args.crash_at == CrashAt::MidReplay {
        let logged = count_completed_steps(&args.journal)?;
        let journal = FileJournal::open(&args.journal)?;
        let mut ctx = DurableContext::new(journal)?;
        // Replay only half of the logged steps, then exit.
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

    // MidWrite: crash "in the middle" of writing the last step.
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
                // MidWrite: write a torn last line and exit.
                // The previously completed steps are already on disk
                // (intact lines); a genuine torn line is appended
                // directly to the file.
                drop(ctx);
                write_torn_line(&args.journal, &step_name(&args.task, index))?;
                eprintln!("crash injected: mid_write (torn last line at step {index})");
                std::process::exit(137);
            }

            let name = step_name(&args.task, index);
            let payload = step_payload(&args.task, index);
            // Durable step: on a fresh run, the closure runs and the
            // result is recorded.
            let recorded: String = ctx.step(&name, move || Ok(payload))?;

            // The side effect (memory write) only runs on a fresh run —
            // turn_key makes it idempotent across replay.
            persist_step_memory(&store, &args.task, index, &recorded, clock).await?;
        }
        // ctx drops here; the journal has already been flushed on every step.
    }

    eprintln!(
        "start complete: {} step(s) for task {}",
        args.steps, args.task
    );
    Ok(())
}

/// Writes a genuinely torn last line to the journal file.
///
/// This produces the classic "crash mid-write" state: an incomplete JSON
/// object with no trailing newline at the end of the file.
/// [`DurableContext::new`] skips this (the line doesn't parse as
/// `StepCompleted`), so resume continues from the correct step.
fn write_torn_line(journal: &PathBuf, step: &str) -> DaemonResult<()> {
    use std::fs::OpenOptions;
    let mut f = OpenOptions::new().append(true).create(true).open(journal)?;
    // Incomplete line: starts like a real entry but breaks off mid-way —
    // NO trailing newline. (The EntryKind serde tag is "kind"=snake_case;
    // the line breaks before the closing brace.)
    write!(
        f,
        "{{\"step_id\":999,\"timestamp\":\"2026\",\"kind\":\"step_completed\",\"name\":\"{step}\",\"out"
    )?;
    f.flush()?;
    Ok(())
}

/// Handles `resume`: replay from the journal + fresh steps, report.
async fn run_resume(args: ResumeArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;
    let store = Arc::new(LocalJsonStore::open(&args.store).await?);

    let journal = FileJournal::open(&args.journal)?;
    let mut ctx = DurableContext::new(journal)?;
    let was_replaying = ctx.is_replaying();
    let replayed_before = ctx.steps_taken();
    // Size of the replay vector = how many steps were in the log before
    // the fresh run.
    let steps_in_log = count_completed_steps(&args.journal)?;

    let mut fresh_steps = 0usize;
    for index in 0..args.steps {
        let was_fresh = !ctx.is_replaying();
        let name = step_name(&args.task, index);
        let payload = step_payload(&args.task, index);
        let recorded: String = ctx.step(&name, move || Ok(payload))?;

        // ADR: Dual-write fix — always persist memory for steps recorded in journal.
        // turn_key makes this idempotent (safe to call during replay).
        persist_step_memory(&store, &args.task, index, &recorded, clock).await?;

        if was_fresh {
            fresh_steps += 1;
        }
    }

    // The end state is "clean" if all steps have now run and the memory
    // store has exactly `steps` memories for this task.
    let task_memories = count_task_memories(&store, &args.task).await?;
    let resumed_clean = ctx.steps_taken() == args.steps && task_memories == args.steps;

    let _ = replayed_before; // always 0 (the cursor starts at zero)
    let output = ResumeOutput {
        steps_replayed: steps_in_log.min(args.steps),
        was_replaying,
        fresh_steps,
        resumed_clean,
    };
    emit(&output)
}

/// Handles `recall`: queries the store and prints the hits.
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

/// Handles `sleep`: runs a single sleep cycle and prints the summary.
async fn run_sleep(args: SleepArgs) -> DaemonResult<()> {
    let clock = parse_clock(&args.clock)?;
    let store = LocalJsonStore::open(&args.store).await?;

    // State of the protected anchors before sleep (to prove integrity).
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

/// Records one step's memory in the store idempotently (`turn_key`).
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
    // Idempotence: same task+step → same key → no duplicate on replay.
    memory.turn_key = Some(format!("{task}:step-{index}"));
    store.add(memory).await?;
    Ok(())
}

/// Counts the active memories belonging to a given task (marked by tag).
async fn count_task_memories(store: &Arc<LocalJsonStore>, task: &str) -> DaemonResult<usize> {
    let tag = format!("task:{task}");
    let all = store.all().await?;
    Ok(all
        .into_iter()
        .filter(|m| m.tags.iter().any(|t| t == &tag))
        .count())
}

/// Counts active protected-core (`ProtectedCore`) memories.
async fn count_protected_active(store: &LocalJsonStore) -> DaemonResult<usize> {
    use familyclaw_memory::MemoryStatus;
    let all = store.all().await?;
    Ok(all
        .into_iter()
        .filter(|m| m.decay_policy.is_protected() && m.status == MemoryStatus::Active)
        .count())
}

/// Counts the journal's `StepCompleted` lines (torn incomplete lines
/// don't parse).
fn count_completed_steps(journal: &PathBuf) -> DaemonResult<usize> {
    if !journal.exists() {
        return Ok(0);
    }
    let j = FileJournal::open(journal)?;
    let entries = j.replay_all()?;
    Ok(entries.iter().filter(|e| e.kind.is_step()).count())
}

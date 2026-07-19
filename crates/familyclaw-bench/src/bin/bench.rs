//! `bench` — CLI for the FamilyClaw continuity benchmark.
//!
//! Runs a single scenario or all of them and writes the scorecard (design
//! §4, §6). `bench all` builds a [`FamilyClawSubject`], runs four scenarios
//! ([`CrashMatrix`], [`RetentionCurve`], [`DreamQuality`], [`EmotionalContagion`])
//! with a fixed **injected clock**, and writes `SCORECARD.md` +
//! `scorecard.json` to `crates/familyclaw-bench/out/` (plus a copy at
//! `docs/SCORECARD.md`).
//!
//! ## Reproducibility (design §2.2, §6)
//! The clock is injected as a constant ([`FIXED_CLOCK_RFC3339`]) — the
//! system clock is never read. Same input → byte-for-byte identical
//! scorecard on every run.
//!
//! Run:
//!   `cargo run -p familyclaw-bench -- all`       (all scenarios, FamilyClaw)
//!   `cargo run -p familyclaw-bench -- s1`        (a single scenario)
//!   `cargo run -p familyclaw-bench -- compare`   (comparison: FamilyClaw vs
//!                                                 competitor-shaped baseline
//!                                                 → `COMPARISON.md`)

// Product names (FamilyClaw, OpenClaw, Letta, Hermes) and CLI examples appear
// in the docs as prose — they are not code symbols, so the doc_markdown
// backtick requirement does not apply to them (same allow as in lib.rs).
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use clap::Parser;

use familyclaw_bench::scenarios::{
    CrashMatrix, DreamQuality, EmotionalContagion, EternalThread, ProvenanceGateScenario,
    RetentionCurve, SemanticRetrieval, WeeklyReviewScenario,
};
use familyclaw_bench::{
    run_security_suite, to_security_markdown, BenchError, ComparativeScorecard, FamilyClawSubject,
    Harness, MarkdownFileSubject, Result, Scenario, Scorecard,
};
use familyclaw_core::time;

/// Fixed injected reference clock (design §6: byte-for-byte reproducible).
///
/// `2026-06-04T12:00:00Z` — every scenario and the scorecard are anchored to
/// this value, so two consecutive runs produce an identical `scorecard.json`.
const FIXED_CLOCK_RFC3339: &str = "2026-06-04T12:00:00Z";

/// Command-line interface for the continuity benchmark.
#[derive(Parser)]
#[command(name = "bench", about = "FamilyClaw continuity benchmark harness")]
struct Cli {
    /// The scenario to run by ID, `all` for every scenario against
    /// FamilyClaw, `compare` to run every scenario against **both** subjects
    /// (FamilyClaw vs a competitor-shaped baseline) and write a comparison
    /// report, or `security` to run the SEC1-SEC4 security scenario suite
    /// (fuel, capability, SSRF, and approval gates) and write the
    /// SECURITY_SCORECARD (e.g. `s1`, `all`, `compare`, `security`).
    #[arg(value_name = "SCENARIO")]
    scenario: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing setup — the `RUST_LOG` environment variable controls the level.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Injected clock — NOT the system clock (reproducibility, design §2.2).
    let clock = time::parse_rfc3339(FIXED_CLOCK_RFC3339)?;

    // `compare` runs the SAME scenario suite against both subjects and writes
    // a comparison report; other identifiers run only against FamilyClaw (as
    // before).
    if cli.scenario == "compare" {
        return run_compare(clock).await;
    }

    // `security` runs the security scenario suite (SEC1-SEC4) and writes
    // SECURITY_SCORECARD.md + json. It does NOT need the continuity_daemon
    // binary (it runs entirely in-process against the real
    // sandbox/actions interface), so it skips `ensure_daemon_env` just like
    // `compare`.
    if cli.scenario == "security" {
        return run_security(clock).await;
    }

    // Select the scenarios to run based on the identifier.
    let scenarios = select_scenarios(&cli.scenario)?;

    // Build the FamilyClaw subject (runs the continuity_daemon binary as a
    // black box). The binary's path is located from the environment; make
    // sure it is built and can be found before running.
    ensure_daemon_env()?;
    let mut subject = FamilyClawSubject::from_env()?;

    tracing::info!(
        scenario = %cli.scenario,
        clock = %FIXED_CLOCK_RFC3339,
        "running continuity benchmark"
    );

    let card = Harness::new().run(&mut subject, &scenarios, clock).await?;

    write_outputs(&card, &cli.scenario)?;

    // Print a short summary to stdout (for humans); the machine-readable
    // artifact is scorecard.json.
    println!("{}", card.to_markdown());

    if card.all_passed() {
        tracing::info!("benchmark complete: ALL PASSED");
    } else {
        // CI gate: a failed scorecard ALSO fails the process (not just a
        // warning) — otherwise 6/6 → 0/6 would still show green in CI, since
        // this exit code is the only gate (ci.yml does not run the bench
        // separately).
        tracing::error!("benchmark complete: SOME SCENARIOS FAILED");
        return Err(BenchError::scenario(
            "benchmark failed: one or more scenarios did not pass",
        ));
    }

    Ok(())
}

/// Runs every scenario against **both** subjects and writes the comparison
/// report (`COMPARISON.md`).
///
/// FamilyClaw is run against the `continuity_daemon` binary (the same black
/// box as in an `all` run); the competitor-shaped baseline
/// ([`MarkdownFileSubject`]) runs purely in-process. Both get the **same**
/// scenario suite and the **same** injected clock, so the output is
/// byte-for-byte reproducible (design §6).
///
/// # Errors
/// [`BenchError`] if the daemon binary cannot be found or a scenario/write
/// fails.
async fn run_compare(clock: familyclaw_core::Timestamp) -> Result<()> {
    tracing::info!(
        clock = %FIXED_CLOCK_RFC3339,
        "running COMPARATIVE continuity benchmark (FamilyClaw vs baseline)"
    );

    // FamilyClaw — the daemon binary as a black box.
    ensure_daemon_env()?;
    let mut familyclaw = FamilyClawSubject::from_env()?;
    let fc_card = Harness::new()
        .run(&mut familyclaw, &select_scenarios("all")?, clock)
        .await?;

    // Competitor-shaped baseline — purely in-process (no daemon).
    // A fresh scenario suite: `Box<dyn Scenario>` is consumed during the run.
    let mut baseline = MarkdownFileSubject::new();
    let base_card = Harness::new()
        .run(&mut baseline, &select_scenarios("all")?, clock)
        .await?;

    let comparison = ComparativeScorecard::new(fc_card, base_card, clock);

    write_comparison(&comparison)?;

    // Print the comparison to stdout (for humans).
    println!("{}", comparison.to_markdown());

    if comparison.familyclaw_wins_crash_matrix() {
        tracing::info!(
            "comparison complete: FamilyClaw WINS crash_matrix \
             (side_effect_overcount 0 vs >0)"
        );
    } else {
        tracing::warn!("comparison complete: FamilyClaw advantage NOT established this run");
    }

    Ok(())
}

/// Runs the security scenario suite (SEC1-SEC4) and writes the security
/// scorecard.
///
/// The suite runs entirely in-process against the real sandbox/actions
/// interface (no daemon). Output is written to `out/SECURITY_SCORECARD.md` +
/// `out/security_scorecard.json` (plus a copy at
/// `docs/SECURITY_SCORECARD.md`). The process returns `Err` if any scenario
/// did not pass, so CI can gate on it (the same pattern as the continuity
/// bench).
///
/// # Errors
/// [`BenchError`] if a scenario cannot run or the write fails, or if any
/// security scenario failed (`passed = false`).
async fn run_security(clock: familyclaw_core::Timestamp) -> Result<()> {
    tracing::info!(
        clock = %FIXED_CLOCK_RFC3339,
        "running SECURITY benchmark (SEC1 fuel, SEC2 capability, SEC3 SSRF, SEC4 approval)"
    );

    let card = run_security_suite(clock).await?;

    write_security_outputs(&card)?;

    // Print the security markdown to stdout (for humans).
    println!("{}", to_security_markdown(&card));

    if card.all_passed() {
        tracing::info!("security benchmark complete: ALL SCENARIOS PASSED (0 escapes)");
        Ok(())
    } else {
        // CI gate: a failed security scenario fails the process.
        tracing::error!("security benchmark complete: SOME SCENARIOS FAILED");
        Err(BenchError::scenario(
            "security benchmark failed: one or more scenarios did not pass",
        ))
    }
}

/// Writes the security scorecard to the `out/` directory and the public
/// `docs/` directory.
///
/// # Errors
/// [`BenchError::Io`]/[`BenchError::Serde`] if the write or serialization fails.
fn write_security_outputs(card: &Scorecard) -> Result<()> {
    let root = workspace_crate_root();
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;

    let json = card.to_json()?;
    let md = to_security_markdown(card);

    write_atomic(&out_dir.join("security_scorecard.json"), json.as_bytes())?;
    write_atomic(&out_dir.join("SECURITY_SCORECARD.md"), md.as_bytes())?;

    // Public artifact in the repo's `docs/` directory (alongside SCORECARD.md).
    if let Some(docs_dir) = root
        .parent()
        .and_then(Path::parent)
        .map(|ws| ws.join("docs"))
    {
        std::fs::create_dir_all(&docs_dir)?;
        write_atomic(&docs_dir.join("SECURITY_SCORECARD.md"), md.as_bytes())?;
    }

    tracing::info!(out = %out_dir.display(), "security scorecard written");
    Ok(())
}

/// Builds the scenarios to run from the identifier.
///
/// `all` runs S1+S2+S3 in a fixed order. Individual identifiers
/// (`s1`/`s2`/`s3` or the full `s1_crash_matrix` etc.) run only one.
///
/// # Errors
/// [`BenchError::Scenario`] if the identifier is unknown.
fn select_scenarios(id: &str) -> Result<Vec<Box<dyn Scenario>>> {
    let s1 = || -> Box<dyn Scenario> { Box::new(CrashMatrix::new()) };
    let s2 = || -> Box<dyn Scenario> { Box::new(RetentionCurve::new()) };
    let s3 = || -> Box<dyn Scenario> { Box::new(DreamQuality::new()) };
    let s4 = || -> Box<dyn Scenario> { Box::new(EmotionalContagion::new()) };
    let s5 = || -> Box<dyn Scenario> { Box::new(SemanticRetrieval::new()) };
    let s6 = || -> Box<dyn Scenario> { Box::new(EternalThread::new()) };
    let s7 = || -> Box<dyn Scenario> { Box::new(ProvenanceGateScenario::new()) };
    let s8 = || -> Box<dyn Scenario> { Box::new(WeeklyReviewScenario::new()) };

    match id {
        "all" => Ok(vec![s1(), s2(), s3(), s4(), s5(), s6(), s7(), s8()]),
        "s1" | "s1_crash_matrix" => Ok(vec![s1()]),
        "s2" | "s2_retention_curve" => Ok(vec![s2()]),
        "s3" | "s3_dream_quality" => Ok(vec![s3()]),
        "s4" | "s4_emotional_contagion" => Ok(vec![s4()]),
        "s5" | "s5_semantic_retrieval" => Ok(vec![s5()]),
        "s6" | "s6_eternal_thread" => Ok(vec![s6()]),
        "s7" | "s7_provenance_gate" => Ok(vec![s7()]),
        "s8" | "s8_weekly_review" => Ok(vec![s8()]),
        other => Err(BenchError::scenario(format!(
            "unknown scenario '{other}' (expected: all, s1, s2, s3, s4, s5, s6, s7, s8)"
        ))),
    }
}

/// Ensures that the `continuity_daemon` binary can be found: if
/// `CONTINUITY_DAEMON_BIN` is not set, derives it from the current binary's
/// location (`target/<profile>/`) and sets the environment variable.
///
/// `cargo run -p familyclaw-bench` builds the `bench` binary into
/// `target/<profile>/`, where `continuity_daemon` also lives (workspace
/// binaries share the same directory).
///
/// # Errors
/// [`BenchError::Subject`] if the binary cannot be found anywhere.
fn ensure_daemon_env() -> Result<()> {
    // An explicit override wins — don't touch it if already set.
    if std::env::var("CONTINUITY_DAEMON_BIN").is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .map_err(|e| BenchError::subject(format!("current_exe failed: {e}")))?;
    // exe = target/<profile>/bench(.exe) → profile directory = exe.parent().
    let profile_dir = exe
        .parent()
        .ok_or_else(|| BenchError::subject("bench binary has no parent dir"))?;
    let mut bin = profile_dir.join("continuity_daemon");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    if !bin.exists() {
        return Err(BenchError::subject(format!(
            "continuity_daemon not found at {} — run \
             `cargo build -p familyclaw-agent --bin continuity_daemon` first \
             (or set CONTINUITY_DAEMON_BIN)",
            bin.display()
        )));
    }
    std::env::set_var("CONTINUITY_DAEMON_BIN", &bin);
    Ok(())
}

/// Writes the scorecard to both the `out/` directory and `docs/SCORECARD.md`.
///
/// `scorecard.json` is written only on an `all` run (the full scorecard);
/// individual scenario runs only write the markdown for diagnostics.
///
/// # Errors
/// [`BenchError::Io`]/[`BenchError::Serde`] if the write or serialization fails.
fn write_outputs(card: &Scorecard, scenario: &str) -> Result<()> {
    let root = workspace_crate_root();
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;

    let json = card.to_json()?;
    let md = card.to_markdown();

    // Byte-for-byte deterministic JSON is written without a trailing
    // newline, so comparing two runs is a direct byte comparison (design §6).
    write_atomic(&out_dir.join("scorecard.json"), json.as_bytes())?;
    write_atomic(&out_dir.join("SCORECARD.md"), md.as_bytes())?;

    // Public artifact in the repo's `docs/` directory (design §4).
    if scenario == "all" {
        let docs_dir = root
            .parent()
            .and_then(Path::parent)
            .map(|ws| ws.join("docs"));
        if let Some(docs_dir) = docs_dir {
            std::fs::create_dir_all(&docs_dir)?;
            write_atomic(&docs_dir.join("SCORECARD.md"), md.as_bytes())?;
        }
    }

    tracing::info!(out = %out_dir.display(), "scorecard written");
    Ok(())
}

/// Writes the comparison report (`COMPARISON.md`) to both the `out/`
/// directory and the public `docs/` directory (same pattern as
/// [`write_outputs`]).
///
/// # Errors
/// [`BenchError::Io`] if directory creation or the write fails.
fn write_comparison(comparison: &ComparativeScorecard) -> Result<()> {
    let root = workspace_crate_root();
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;

    let md = comparison.to_markdown();
    write_atomic(&out_dir.join("COMPARISON.md"), md.as_bytes())?;

    // A public artifact in the repo's `docs/` directory (alongside SCORECARD.md).
    if let Some(docs_dir) = root
        .parent()
        .and_then(Path::parent)
        .map(|ws| ws.join("docs"))
    {
        std::fs::create_dir_all(&docs_dir)?;
        write_atomic(&docs_dir.join("COMPARISON.md"), md.as_bytes())?;
    }

    tracing::info!(out = %out_dir.display(), "comparison written");
    Ok(())
}

/// Writes a file's contents (overwriting). A small helper factored out for
/// consistent error handling.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Returns the `familyclaw-bench` crate's root (`CARGO_MANIFEST_DIR`).
///
/// This is a compile-time constant that always points to
/// `crates/familyclaw-bench/` regardless of the working directory —
/// `out/` is written here deterministically.
fn workspace_crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

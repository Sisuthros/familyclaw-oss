//! Cron-compatible entrypoint for dream cycles.
//!
//! This module provides a command-line tool that:
//!
//! 1. Computes the most recent dream cycle time ([`DesireClock`])
//! 2. Checks whether it has already run ([`DurableContext`] logic)
//! 3. If not, runs [`DreamCycle`] and records the result to the durable log
//!
//! Usage:
//! ```bash
//! # Runs the dream cycle if the most recent one was missed
//! cargo run --bin dream-cron-job
//! ```
//!
//! Environment variables:
//! - `FAMILYCLAW_DATA_DIR` — directory where `memory.json` and `journal.jsonl` are located (required)
//! - `FAMILYCLAW_AGENT_NAME` — agent name for logging (default: "dream")
//! - `FAMILYCLAW_PROFILE_DIR` — profile directory (optional, not read for SOUL in the MVP)
//! - `RUST_LOG` — log level (default: info)

use std::sync::Arc;

use familyclaw_core::{time, FamilyClawError, Result};
use familyclaw_dream::{desire_clock::DesireClock, DreamConfig, DreamCycle};
use familyclaw_durable::{context::DurableContext, FileJournal};
use familyclaw_memory::LocalJsonStore;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Käynnistetään unijakso...");

    // Read the data directory from the environment
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR").map_err(|_| {
        FamilyClawError::config(
            "FAMILYCLAW_DATA_DIR ei asetettu — vaaditaan memory.json ja journal.jsonl",
        )
    })?;

    let data_path = std::path::Path::new(&data_dir);
    std::fs::create_dir_all(data_path).map_err(|e| {
        FamilyClawError::config(format!(
            "FAMILYCLAW_DATA_DIR hakemiston luonti epäonnistui: {e}"
        ))
    })?;

    let journal_path = data_path.join("journal.jsonl");
    let memory_path = data_path.join("memory.json");

    // Open (or create) the stores — no pre-existing files required.
    let journal = Arc::new(FileJournal::open(&journal_path)?);
    let store = Arc::new(LocalJsonStore::open(&memory_path).await?);

    let _agent_name =
        std::env::var("FAMILYCLAW_AGENT_NAME").unwrap_or_else(|_| "dream".to_string());

    let now = time::now();
    let _clock = DesireClock::default();

    // Check whether the dream cycle has already run tonight (per-day idempotence)
    let mut context = DurableContext::new(journal.clone())?;
    let step_name = format!("dream_cycle:{}", now.format("%Y-%m-%d"));
    let already_run = context.has_run_step(&step_name)?;

    if already_run {
        tracing::info!(%step_name, "Unijakso on jo ajettu tänä yölle — ohitetaan");
        println!("Unijakso on jo ajettu tänä yölle — ohitetaan");
        return Ok(());
    }

    // Run the dream cycle
    tracing::info!(%step_name, "Ajetaan unijakso...");
    println!("Ajetaan unijakso...");

    let cycle = DreamCycle::with_config(store.as_ref(), DreamConfig::default());

    // Execute the dream cycle
    let report = cycle.run(&*journal, now).await?;

    tracing::info!(
        scanned = report.scanned,
        merged = report.merged,
        dropped = report.dropped,
        dates_absolutized = report.dates_absolutized,
        strengthened = report.strengthened,
        archived = report.archived,
        "Unijakso ajettu onnistuneesti"
    );

    println!(
        "Unijakso valmis: skannattu={}, yhdistetty={}, pudotettu={}, päivät absolutisoitu={}, vahvistettu={}, arkistoitu={}",
        report.scanned, report.merged, report.dropped,
        report.dates_absolutized, report.strengthened, report.archived,
    );

    // Record the step in the durable context (idempotent via turn_key)
    context.step(&step_name, move || Ok(report))?;

    Ok(())
}

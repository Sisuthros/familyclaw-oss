//! Cron-yhteensopiva entrypoint unijaksoille.
//!
//! Tämä moduuli tarjoaa komentorivityökalun joka:
//!
//! 1. Laskee viimeisimmän unijakson ajankohdan ([`DesireClock`])
//! 2. Tarkistaa onko se jo ajettu ([`DurableContext`] logiikka)
//! 3. Jos ei, ajaa [`DreamCycle`] ja kirjaa tuloksen durable-lokiin
//!
//! Käyttö:
//! ```bash
//! # Ajaa unijakson jos viimeisin jäi väliin
//! cargo run --bin dream-cron-job
//! ```
//!
//! Ympäristömuuttujat:
//! - `FAMILYCLAW_DATA_DIR` — hakemisto jossa `memory.json` ja `journal.jsonl` sijaitsevat (pakollinen)
//! - `FAMILYCLAW_AGENT_NAME` — agentin nimi lokitusta varten (oletus: "dream")
//! - `FAMILYCLAW_PROFILE_DIR` — profiilikansio (valinnainen, tämä ei lue SOUL:ia MVP:ssä)
//! - `RUST_LOG` — logitaso (oletus: info)

use std::sync::Arc;

use familyclaw_core::{time, Result};
use familyclaw_dream::{desire_clock::DesireClock, DreamConfig, DreamCycle};
use familyclaw_durable::{context::DurableContext, FileJournal};
use familyclaw_memory::LocalJsonStore;
use tokio;

#[tokio::main]
async fn main() -> Result<()> {
    // Alusta lokitus
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Käynnistetään unijakso...");

    // Lue data-hakemisto ympäristöstä
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR")
        .map_err(|_| anyhow::anyhow!("FAMILYCLAW_DATA_DIR ei asetettu — vaaditaan memory.json ja journal.jsonl"))?;

    let data_path = std::path::Path::new(&data_dir);
    std::fs::create_dir_all(data_path)
        .map_err(|e| anyhow::anyhow!("FAMILYCLAW_DATA_DIR hakemiston luonti epäonnistui: {e}"))?;

    let journal_path = data_path.join("journal.jsonl");
    let memory_path = data_path.join("memory.json");

    // Avaa (tai luo) tallennukset — ei vaadi valmiita tiedostoja etukäteen.
    let journal = Arc::new(FileJournal::open(&journal_path)?);
    let store = Arc::new(LocalJsonStore::open(&memory_path).await?);

    let _agent_name = std::env::var("FAMILYCLAW_AGENT_NAME").unwrap_or_else(|_| "dream".to_string());

    let now = time::now();
    let _clock = DesireClock::default();

    // Tarkista onko unijakso jo ajettu tänä yölle (päiväkohtainen idempotenssi)
    let mut context = DurableContext::new(journal.clone())?;
    let step_name = format!("dream_cycle:{}", chrono::DateTime::<chrono::Utc>::from(now).format("%Y-%m-%d"));
    let already_run = context.has_run_step(&step_name)?;

    if already_run {
        tracing::info!(%step_name, "Unijakso on jo ajettu tänä yölle — ohitetaan");
        println!("Unijakso on jo ajettu tänä yölle — ohitetaan");
        return Ok(());
    }

    // Aja unijakso
    tracing::info!(%step_name, "Ajetaan unijakso...");
    println!("Ajetaan unijakso...");

    let cycle = DreamCycle::with_config(store.as_ref(), DreamConfig::default());

    // Suorita unijakso
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

    // Kirjaa askel durable-kontekstiin (idempotentti turn_keyn kautta)
    context.step(&step_name, move || Ok(report))?;

    Ok(())
}
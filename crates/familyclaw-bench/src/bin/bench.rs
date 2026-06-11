//! `bench` — FamilyClaw-jatkuvuusbenchmarkin CLI.
//!
//! Ajaa yhden skenaarion tai kaikki ja kirjoittaa scorecardin (design §4, §6).
//! `bench all` rakentaa [`FamilyClawSubject`]:n, ajaa neljä skenaariota
//! ([`CrashMatrix`], [`RetentionCurve`], [`DreamQuality`], [`EmotionalContagion`])
//! kiinteällä
//! **injektoidulla kellolla** ja kirjoittaa `SCORECARD.md` + `scorecard.json`
//! hakemistoon `crates/familyclaw-bench/out/` (sekä kopion `docs/SCORECARD.md`).
//!
//! ## Reprodusoitavuus (design §2.2, §6)
//! Kello injektoidaan vakiona ([`FIXED_CLOCK_RFC3339`]) — järjestelmäkelloa ei
//! lueta. Sama syöte → tavu-tavulta identtinen scorecard joka ajolla.
//!
//! Aja:
//!   `cargo run -p familyclaw-bench -- all`       (kaikki, FamilyClaw)
//!   `cargo run -p familyclaw-bench -- s1`        (yksittäinen skenaario)
//!   `cargo run -p familyclaw-bench -- compare`   (vertailu: FamilyClaw vs
//!                                                 kilpailijan-muotoinen perustaso
//!                                                 → `COMPARISON.md`)

// Tuotenimet (FamilyClaw, OpenClaw, Letta, Hermes) ja CLI-esimerkit esiintyvät
// dokumentaatiossa proosana — ne eivät ole koodisymboleita, joten
// doc_markdown-backtick-vaatimus ei koske niitä (sama allow kuin lib.rs:ssä).
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use clap::Parser;

use familyclaw_bench::scenarios::{
    CrashMatrix, DreamQuality, EmotionalContagion, EternalThread, RetentionCurve, SemanticRetrieval,
};
use familyclaw_bench::{
    BenchError, ComparativeScorecard, FamilyClawSubject, Harness, MarkdownFileSubject, Result,
    Scenario, Scorecard,
};
use familyclaw_core::time;

/// Kiinteä injektoitu referenssikello (design §6: reprodusoitava byte-for-byte).
///
/// `2026-06-04T12:00:00Z` — kaikki skenaariot ja scorecard ankkuroidaan tähän,
/// jolloin kaksi peräkkäistä ajoa tuottaa identtisen `scorecard.json`:n.
const FIXED_CLOCK_RFC3339: &str = "2026-06-04T12:00:00Z";

/// Jatkuvuusbenchmarkin komentorivikäyttöliittymä.
#[derive(Parser)]
#[command(name = "bench", about = "FamilyClaw continuity benchmark harness")]
struct Cli {
    /// Ajettava skenaario tunnisteella, `all` kaikille FamilyClawlla, tai
    /// `compare` ajamaan kaikki skenaariot **molemmilla** subjekteilla
    /// (FamilyClaw vs kilpailijan-muotoinen perustaso) ja kirjoittamaan
    /// vertailuraportti (esim. `s1`, `all`, `compare`).
    #[arg(value_name = "SCENARIO")]
    scenario: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing-alustus — `RUST_LOG`-ympäristömuuttuja ohjaa tasoa.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Injektoitu kello — EI järjestelmäkello (reprodusoitavuus, design §2.2).
    let clock = time::parse_rfc3339(FIXED_CLOCK_RFC3339)?;

    // `compare` ajaa SAMAN skenaariosarjan molemmilla subjekteilla ja kirjoittaa
    // vertailuraportin; muut tunnisteet ajavat vain FamilyClawn (kuten ennen).
    if cli.scenario == "compare" {
        return run_compare(clock).await;
    }

    // Valitse ajettavat skenaariot tunnisteen perusteella.
    let scenarios = select_scenarios(&cli.scenario)?;

    // Rakenna FamilyClaw-subjekti (ajaa continuity_daemon-binääriä mustana
    // laatikkona). Binäärin polku paikannetaan ympäristöstä; varmistetaan että
    // se on rakennettu ja löydettävissä ennen ajoa.
    ensure_daemon_env()?;
    let mut subject = FamilyClawSubject::from_env()?;

    tracing::info!(
        scenario = %cli.scenario,
        clock = %FIXED_CLOCK_RFC3339,
        "running continuity benchmark"
    );

    let card = Harness::new().run(&mut subject, &scenarios, clock).await?;

    write_outputs(&card, &cli.scenario)?;

    // Tulosta lyhyt yhteenveto stdoutiin (ihmiselle); koneluettava artefakti on
    // scorecard.json.
    println!("{}", card.to_markdown());

    if card.all_passed() {
        tracing::info!("benchmark complete: ALL PASSED");
    } else {
        tracing::warn!("benchmark complete: SOME SCENARIOS FAILED");
    }

    Ok(())
}

/// Ajaa kaikki skenaariot **molemmilla** subjekteilla ja kirjoittaa
/// vertailuraportin (`COMPARISON.md`).
///
/// FamilyClaw ajetaan `continuity_daemon`-binääriä vasten (sama musta laatikko
/// kuin `all`-ajossa); kilpailijan-muotoinen perustaso
/// ([`MarkdownFileSubject`]) ajetaan puhtaasti in-process. Molemmat saavat
/// **saman** skenaariosarjan ja **saman** injektoidun kellon, joten tuloste on
/// tavu-tavulta reprodusoitava (design §6).
///
/// # Errors
/// [`BenchError`] jos daemon-binääriä ei löydy tai jokin skenaario/kirjoitus
/// epäonnistuu.
async fn run_compare(clock: familyclaw_core::Timestamp) -> Result<()> {
    tracing::info!(
        clock = %FIXED_CLOCK_RFC3339,
        "running COMPARATIVE continuity benchmark (FamilyClaw vs baseline)"
    );

    // FamilyClaw — daemon-binääri mustana laatikkona.
    ensure_daemon_env()?;
    let mut familyclaw = FamilyClawSubject::from_env()?;
    let fc_card = Harness::new()
        .run(&mut familyclaw, &select_scenarios("all")?, clock)
        .await?;

    // Kilpailijan-muotoinen perustaso — puhdas in-process (ei daemonia).
    // Tuore skenaariosarja: `Box<dyn Scenario>` kulutetaan ajossa.
    let mut baseline = MarkdownFileSubject::new();
    let base_card = Harness::new()
        .run(&mut baseline, &select_scenarios("all")?, clock)
        .await?;

    let comparison = ComparativeScorecard::new(fc_card, base_card, clock);

    write_comparison(&comparison)?;

    // Tulosta vertailu stdoutiin (ihmiselle).
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

/// Rakentaa ajettavat skenaariot tunnisteesta.
///
/// `all` ajaa S1+S2+S3 kiinteässä järjestyksessä. Yksittäiset tunnisteet
/// (`s1`/`s2`/`s3` tai täysi `s1_crash_matrix` jne.) ajavat vain yhden.
///
/// # Errors
/// [`BenchError::Scenario`] jos tunniste on tuntematon.
fn select_scenarios(id: &str) -> Result<Vec<Box<dyn Scenario>>> {
    let s1 = || -> Box<dyn Scenario> { Box::new(CrashMatrix::new()) };
    let s2 = || -> Box<dyn Scenario> { Box::new(RetentionCurve::new()) };
    let s3 = || -> Box<dyn Scenario> { Box::new(DreamQuality::new()) };
    let s4 = || -> Box<dyn Scenario> { Box::new(EmotionalContagion::new()) };
    let s5 = || -> Box<dyn Scenario> { Box::new(SemanticRetrieval::new()) };
    let s6 = || -> Box<dyn Scenario> { Box::new(EternalThread::new()) };

    match id {
        "all" => Ok(vec![s1(), s2(), s3(), s4(), s5(), s6()]),
        "s1" | "s1_crash_matrix" => Ok(vec![s1()]),
        "s2" | "s2_retention_curve" => Ok(vec![s2()]),
        "s3" | "s3_dream_quality" => Ok(vec![s3()]),
        "s4" | "s4_emotional_contagion" => Ok(vec![s4()]),
        "s5" | "s5_semantic_retrieval" => Ok(vec![s5()]),
        "s6" | "s6_eternal_thread" => Ok(vec![s6()]),
        other => Err(BenchError::scenario(format!(
            "unknown scenario '{other}' (expected: all, s1, s2, s3, s4, s5, s6)"
        ))),
    }
}

/// Varmistaa että `continuity_daemon`-binääri löytyy: jos `CONTINUITY_DAEMON_BIN`
/// ei ole asetettu, johtaa sen nykyisen binäärin sijainnista (`target/<profile>/`)
/// ja asettaa ympäristömuuttujan.
///
/// `cargo run -p familyclaw-bench` rakentaa `bench`-binäärin
/// `target/<profile>/`-hakemistoon, jossa myös `continuity_daemon` sijaitsee
/// (workspace-binäärit jakavat saman hakemiston).
///
/// # Errors
/// [`BenchError::Subject`] jos binääriä ei löydy mistään.
fn ensure_daemon_env() -> Result<()> {
    // Eksplisiittinen yliajo voittaa — älä koske jos jo asetettu.
    if std::env::var("CONTINUITY_DAEMON_BIN").is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .map_err(|e| BenchError::subject(format!("current_exe failed: {e}")))?;
    // exe = target/<profile>/bench(.exe) → profiilihakemisto = exe.parent().
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

/// Kirjoittaa scorecardin sekä `out/`-hakemistoon että `docs/SCORECARD.md`:hen.
///
/// `scorecard.json` kirjoitetaan vain `all`-ajossa (täysi tuloskortti); yksittäiset
/// skenaarioajot kirjoittavat vain markdownin diagnostiikaksi.
///
/// # Errors
/// [`BenchError::Io`]/[`BenchError::Serde`] jos kirjoitus tai sarjallistus epäonnistuu.
fn write_outputs(card: &Scorecard, scenario: &str) -> Result<()> {
    let root = workspace_crate_root();
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;

    let json = card.to_json()?;
    let md = card.to_markdown();

    // Tavu-tavulta deterministinen JSON kirjoitetaan ilman lopun rivinvaihtoa,
    // jotta kahden ajon vertailu on suora byte-vertailu (design §6).
    write_atomic(&out_dir.join("scorecard.json"), json.as_bytes())?;
    write_atomic(&out_dir.join("SCORECARD.md"), md.as_bytes())?;

    // Julkinen artefakti repon `docs/`-hakemistoon (design §4).
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

/// Kirjoittaa vertailuraportin (`COMPARISON.md`) sekä `out/`-hakemistoon että
/// julkiseen `docs/`-hakemistoon (sama kuvio kuin [`write_outputs`]).
///
/// # Errors
/// [`BenchError::Io`] jos hakemiston luonti tai kirjoitus epäonnistuu.
fn write_comparison(comparison: &ComparativeScorecard) -> Result<()> {
    let root = workspace_crate_root();
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;

    let md = comparison.to_markdown();
    write_atomic(&out_dir.join("COMPARISON.md"), md.as_bytes())?;

    // Julkinen artefakti repon `docs/`-hakemistoon (rinnan SCORECARD.md:n kanssa).
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

/// Kirjoittaa tiedoston sisällön (ylikirjoittaa). Eristetty apuri yhtenäistä
/// virheenkäsittelyä varten.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Palauttaa `familyclaw-bench`-craten juuren (`CARGO_MANIFEST_DIR`).
///
/// Tämä on käännösaikainen vakio joka osoittaa aina `crates/familyclaw-bench/`:iin
/// riippumatta ajohakemistosta — `out/` kirjoitetaan tänne deterministisesti.
fn workspace_crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

//! `gemu` — Geminin rälläkkäliittymä `FamilyClaw`'n rasvamonttuun.
//!
//! ## Komennot
//!
//! ```bash
//! # Skannaa koodipuu (ei kutsu Geminiä)
//! gemu scan
//!
//! # Syötä tehtävä Geminille (kertakäyttö)
//! gemu run "lisää validointi agentin handle_turn-metodiin"
//!
//! # Syötä tehtävä + anna Geminille lupa ajaa komentoja
//! gemu run --yolo "korjaa kaikki clippy-varoitukset"
//!
//! # Interaktiivinen sessio
//! gemu chat
//!
//! # Tarkista Gemini CLI:n versio
//! gemu version
//! ```
//!
//! ## Autentikaatio
//!
//! Gemu käyttää Gemini CLI:n OAuth-autentikaatiota.
//! Ensimmäisellä kerralla Gemini CLI avaa selaimen kirjautumista varten.
//!
//! API-avaimen voi myös asettaa: `export GEMINI_API_KEY=...`

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use familyclaw_gemu::{context, gemini, prompt, GemuConfig};

/// Gemu — Pääkatsastusmies Kardaani-Jordaanin varikolta. 🏎️🔧
#[derive(Parser)]
#[command(name = "gemu", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Skannaa koodipuun ja näyttää rakenteen (ei kutsu Geminiä).
    Scan {
        /// Polku projektiin (oletus: cwd)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Syötä tehtävä Geminille koko koodipuun kontekstin kanssa (kertakäyttö).
    Run {
        /// Tehtävän kuvaus suomeksi tai englanniksi.
        task: Vec<String>,

        /// Polku projektiin (oletus: cwd)
        #[arg(short, long, default_value = ".")]
        path: PathBuf,

        /// Malli (oletus: gemini-2.5-pro)
        #[arg(short, long, default_value = "gemini-2.5-pro")]
        model: String,

        /// YOLO-tila — Gemu ajaa komennot automaattisesti ilman varmistusta
        #[arg(short, long)]
        yolo: bool,

        /// Sandbox-tila
        #[arg(short, long)]
        sandbox: bool,

        /// Lisähakemistot joihin Geminillä on pääsy
        #[arg(long = "include")]
        include_dirs: Vec<PathBuf>,

        /// Politiikkatiedosto(t)
        #[arg(long)]
        policy: Vec<PathBuf>,
    },

    /// Interaktiivinen chattisessio Geminin kanssa (koko konteksti ladattuna).
    Chat {
        /// Polku projektiin (oletus: cwd)
        #[arg(short, long, default_value = ".")]
        path: PathBuf,

        /// Malli
        #[arg(short, long, default_value = "gemini-2.5-pro")]
        model: String,

        /// YOLO-tila
        #[arg(short, long)]
        yolo: bool,

        /// Alustava tehtävä/konteksti
        #[arg(short, long)]
        task: Option<String>,
    },

    /// Tarkista Gemini CLI:n versio ja autentikaatio.
    Version,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Scan { path } => cmd_scan(&path)?,
        Command::Run {
            task,
            path,
            model,
            yolo,
            sandbox,
            include_dirs,
            policy,
        } => cmd_run(
            task.join(" "),
            path,
            model,
            yolo,
            sandbox,
            include_dirs,
            policy,
        )?,
        Command::Chat {
            path,
            model,
            yolo,
            task,
        } => cmd_chat(path, model, yolo, task)?,
        Command::Version => cmd_version(),
    }

    Ok(())
}

/// Skannaa ja tulostaa koodipuun.
fn cmd_scan(path: &Path) -> anyhow::Result<()> {
    println!(
        "🔍 Gemu skannaa: {}\n",
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
    );

    let project = context::scan(path)?;
    println!("{}", project.tree);
    println!();
    println!(
        "📊 {} tiedostoa, {} cratea",
        project.total_files,
        project.cargo_tomls.len()
    );
    println!("📄 {} arkkitehtuuridokumenttia", project.arch_docs.len());
    println!();
    println!("Valmiina rälläköintiin. Kokeile: gemu run \"tehtäväsi tähän\"");

    Ok(())
}

/// Suorita tehtävä Geminillä.
fn cmd_run(
    task: String,
    path: PathBuf,
    model: String,
    yolo: bool,
    sandbox: bool,
    include_dirs: Vec<PathBuf>,
    policy: Vec<PathBuf>,
) -> anyhow::Result<()> {
    if task.is_empty() {
        anyhow::bail!("Tehtävä puuttuu. Käytä: gemu run \"mitä pitää tehdä\"");
    }

    // Varmista Gemini CLI
    let version = gemini::check_installed()?;
    println!("🏎️  Gemu käynnistyy (Gemini CLI v{})...", version);
    println!(
        "📂 Projekti: {}",
        path.canonicalize()
            .unwrap_or_else(|_| path.clone())
            .display()
    );

    // Skannaa projekti
    println!("🔍 Skannataan koodipuu...");
    let project = context::scan(&path)?;
    println!(
        "   {} tiedostoa, {} cratea",
        project.total_files,
        project.cargo_tomls.len()
    );

    // Rakenna prompt
    println!("🔧 Rakennetaan Pääkatsastusmies-prompt...");
    let mega_prompt = prompt::build(&project, &task);
    println!(
        "   Prompt: {} merkkiä (n. {} tokenia)",
        mega_prompt.len(),
        mega_prompt.len() / 4
    );

    // Konfiguraatio
    let config = GemuConfig {
        workdir: path.canonicalize().unwrap_or(path),
        model,
        include_dirs,
        yolo,
        sandbox,
        policies: policy,
    };

    // Kutsu Gemini CLI
    println!("🚀 Käynnistetään Gemini...");
    if yolo {
        println!("⚠️  YOLO-tila: Gemu ajaa komennot automaattisesti.");
    }
    println!();

    let exit_code = gemini::run(&mega_prompt, &config, gemini::RunMode::OneShot)?;

    println!();
    if exit_code == 0 {
        println!("✅ Gemu valmis.");
    } else {
        println!("❌ Gemu päättyi virheeseen (exit {exit_code}).");
    }

    Ok(())
}

/// Interaktiivinen sessio.
fn cmd_chat(path: PathBuf, model: String, yolo: bool, task: Option<String>) -> anyhow::Result<()> {
    let version = gemini::check_installed()?;
    println!("🏎️  Gemu chat (Gemini CLI v{version})");
    println!(
        "📂 Projekti: {}",
        path.canonicalize()
            .unwrap_or_else(|_| path.clone())
            .display()
    );

    // Skannaa projekti
    println!("🔍 Skannataan koodipuu...");
    let project = context::scan(&path)?;

    // Rakenna perusprompt
    let base_prompt = if let Some(t) = &task {
        prompt::build(&project, t)
    } else {
        prompt::build(&project, "Tutustu tähän projektiin ja kerro mitä näet.")
    };

    let config = GemuConfig {
        workdir: path.canonicalize().unwrap_or(path),
        model,
        yolo,
        ..Default::default()
    };

    println!("🚀 Käynnistetään interaktiivinen sessio...");
    println!();

    let exit_code = gemini::run(&base_prompt, &config, gemini::RunMode::Interactive)?;

    if exit_code != 0 {
        println!("❌ Sessio päättyi virheeseen (exit {exit_code}).");
    }

    Ok(())
}

/// Versiotarkistus.
fn cmd_version() {
    match gemini::check_installed() {
        Ok(version) => {
            println!("✅ Gemini CLI v{version}");
            println!("   Gemu — Pääkatsastusmies Kardaani-Jordaanin varikolta 🏎️🔧");
            println!();
            println!("Komennot:");
            println!("  gemu scan              Skannaa koodipuu");
            println!("  gemu run \"tehtävä\"     Suorita tehtävä Geminillä");
            println!("  gemu chat              Interaktiivinen sessio");
        }
        Err(e) => {
            println!("❌ {e}");
            println!();
            println!("Asenna Gemini CLI:");
            println!("  npm install -g @google/gemini-cli");
            println!();
            println!("Sitten autentikoi:");
            println!("  gemini          (avaa selaimen kirjautumista varten)");
            println!();
            println!("Tai käytä API-avainta:");
            println!("  export GEMINI_API_KEY=...");
        }
    }
}

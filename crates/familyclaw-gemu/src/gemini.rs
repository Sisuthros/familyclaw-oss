//! Gemini CLI -ajuri — käynnistää Gemini CLI:n aliprosessina.
//!
//! Gemu on ohut wrapper: se kerää kontekstin, rakentaa promptin,
//! ja antaa Gemini CLI:n hoitaa loput (tiedostot, terminaali, agenttisilmukka).

use crate::GemuConfig;
use anyhow::{Context, Result};
use std::process::{Command, Stdio};

/// Ajotila — interaktiivinen vai kertakäyttö.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Kertakäyttö: `-p "prompt"` — Gemini vastaa ja lopettaa.
    OneShot,
    /// Interaktiivinen: `-i "prompt"` — Gemini jatkaa keskustelua.
    Interactive,
}

/// Kutsu Gemini CLI:tä annetulla promptilla ja konfiguraatiolla.
///
/// Palauttaa exit-koodin. Stdout/stderr virtaavat suoraan terminaaliin
/// (peritään), joten käyttäjä näkee Geminin vastauksen reaaliajassa.
pub fn run(prompt: &str, config: &GemuConfig, mode: RunMode) -> Result<i32> {
    let prompt_file = write_temp_prompt(prompt)?;

    let mut cmd = Command::new("gemini");

    // Ajotila
    match mode {
        RunMode::OneShot => {
            cmd.arg("-p");
            cmd.arg(prompt);
        }
        RunMode::Interactive => {
            cmd.arg("-i");
            cmd.arg(prompt);
        }
    }

    // Malli
    cmd.arg("-m");
    cmd.arg(&config.model);

    // YOLO — auto-accept kaikki työkalukutsut
    if config.yolo {
        cmd.arg("--approval-mode");
        cmd.arg("yolo");
    }

    // Sandbox
    if config.sandbox {
        cmd.arg("--sandbox");
    }

    // Lisähakemistot
    for dir in &config.include_dirs {
        cmd.arg("--include-directories");
        cmd.arg(dir);
    }

    // Politiikat
    for policy in &config.policies {
        cmd.arg("--policy");
        cmd.arg(policy);
    }

    // Työhakemisto ja stdio
    cmd.current_dir(&config.workdir);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Aja
    let status = cmd
        .status()
        .with_context(|| "Gemini CLI:n käynnistys epäonnistui. Onko 'gemini' asennettu? (npm install -g @google/gemini-cli)".to_string())?;

    // Siivoa tilapäistiedosto
    let _ = std::fs::remove_file(prompt_file);

    Ok(status.code().unwrap_or(1))
}

/// Tarkista onko Gemini CLI asennettu ja OAuth tehty.
pub fn check_installed() -> Result<String> {
    let output = Command::new("gemini")
        .arg("--version")
        .output()
        .context("Gemini CLI ei löydy. Asenna: npm install -g @google/gemini-cli")?;

    if output.status.success() {
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(version)
    } else {
        Err(anyhow::anyhow!("Gemini CLI ei toimi. Tarkista asennus."))
    }
}

/// Kirjoita prompt tilapäistiedostoon (liian pitkä komentoriville).
fn write_temp_prompt(prompt: &str) -> Result<std::path::PathBuf> {
    let path = std::env::temp_dir().join(format!("gemu-prompt-{}.txt", std::process::id()));
    std::fs::write(&path, prompt)?;
    Ok(path)
}

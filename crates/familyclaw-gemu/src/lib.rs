//! `familyclaw-gemu` — Geminin rälläkkäliittymä `FamilyClaw`'n rasvamonttuun.
//!
//! Gemu on ohut mutta järeä wrapper Google Gemini CLI:n ympärillä:
//! 1. **Skannaa** koodipuun ja arkkitehtuurin
//! 2. **Rakentaa** Pääkatsastusmiehen system promptin + kontekstin
//! 3. **Käynnistää** Gemini CLI:n annetuilla asetuksilla
//!
//! Gemini CLI hoitaa itse OAuth-autentikaation, tiedostojen luvun,
//! terminaalin suorituksen ja agenttisilmukan.

pub mod context;
pub mod gemini;
pub mod prompt;

use std::path::PathBuf;

/// Gemun pääkonfiguraatio.
#[derive(Debug, Clone)]
pub struct GemuConfig {
    /// Työhakemisto (oletus: cwd)
    pub workdir: PathBuf,
    /// Mallin nimi (oletus: "gemini-2.5-pro")
    pub model: String,
    /// Lisähakemistot joista Gemini lukee tiedostoja
    pub include_dirs: Vec<PathBuf>,
    /// YOLO-tila — ei varmistuskyselyjä
    pub yolo: bool,
    /// Sandbox-tila — eristys päällä
    pub sandbox: bool,
    /// Lisäpolitiikkatiedostot
    pub policies: Vec<PathBuf>,
}

impl Default for GemuConfig {
    fn default() -> Self {
        Self {
            workdir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            model: "gemini-2.5-pro".into(),
            include_dirs: Vec::new(),
            yolo: false,
            sandbox: false,
            policies: Vec::new(),
        }
    }
}

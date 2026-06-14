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
    let mut cmd = Command::new("gemini");
    cmd.args(build_args(prompt, config, mode));

    // Työhakemisto ja stdio
    cmd.current_dir(&config.workdir);
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Aja
    let status = cmd
        .status()
        .with_context(|| "Gemini CLI:n käynnistys epäonnistui. Onko 'gemini' asennettu? (npm install -g @google/gemini-cli)".to_string())?;

    Ok(status.code().unwrap_or(1))
}

/// Rakenna Gemini CLI:n komentoriviargumentit promptista ja konfiguraatiosta.
///
/// Eriytetty `run`-funktiosta puhtaana, jotta argumenttien muodostus on
/// testattavissa ilman että `gemini`-binääriä tarvitsee käynnistää.
fn build_args(prompt: &str, config: &GemuConfig, mode: RunMode) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Ajotila — kertakäyttö (-p) vai interaktiivinen (-i).
    let mode_flag = match mode {
        RunMode::OneShot => "-p",
        RunMode::Interactive => "-i",
    };
    args.push(mode_flag.to_string());
    args.push(prompt.to_string());

    // Malli
    args.push("-m".to_string());
    args.push(config.model.clone());

    // YOLO — auto-accept kaikki työkalukutsut
    if config.yolo {
        args.push("--approval-mode".to_string());
        args.push("yolo".to_string());
    }

    // Sandbox
    if config.sandbox {
        args.push("--sandbox".to_string());
    }

    // Lisähakemistot
    for dir in &config.include_dirs {
        args.push("--include-directories".to_string());
        args.push(dir.display().to_string());
    }

    // Politiikat
    for policy in &config.policies {
        args.push("--policy".to_string());
        args.push(policy.display().to_string());
    }

    args
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_config() -> GemuConfig {
        GemuConfig {
            workdir: PathBuf::from("/tmp/work"),
            model: "gemini-2.5-pro".into(),
            include_dirs: Vec::new(),
            yolo: false,
            sandbox: false,
            policies: Vec::new(),
        }
    }

    /// Apuri: löytyykö lippua seuraava arvo argumenttilistasta.
    fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn run_mode_is_copy_and_comparable() {
        let a = RunMode::OneShot;
        let b = a; // Copy — a on yhä käytettävissä.
        assert_eq!(a, b);
        assert_eq!(a, RunMode::OneShot);
        assert_ne!(RunMode::OneShot, RunMode::Interactive);
        // Debug-tuloste on ei-tyhjä.
        assert!(!format!("{a:?}").is_empty());
    }

    #[test]
    fn build_args_oneshot_uses_p_flag() {
        let cfg = base_config();
        let args = build_args("tee jotain", &cfg, RunMode::OneShot);
        assert_eq!(args.first().map(String::as_str), Some("-p"));
        assert_eq!(args.get(1).map(String::as_str), Some("tee jotain"));
        assert!(!args.iter().any(|a| a == "-i"), "ei -i kertakäytössä");
    }

    #[test]
    fn build_args_interactive_uses_i_flag() {
        let cfg = base_config();
        let args = build_args("juttele", &cfg, RunMode::Interactive);
        assert_eq!(args.first().map(String::as_str), Some("-i"));
        assert_eq!(args.get(1).map(String::as_str), Some("juttele"));
        assert!(!args.iter().any(|a| a == "-p"), "ei -p interaktiivisessa");
    }

    #[test]
    fn build_args_includes_model() {
        let mut cfg = base_config();
        cfg.model = "gemini-flash".into();
        let args = build_args("x", &cfg, RunMode::OneShot);
        assert_eq!(flag_value(&args, "-m"), Some("gemini-flash"));
    }

    #[test]
    fn build_args_yolo_disabled_by_default() {
        let cfg = base_config();
        let args = build_args("x", &cfg, RunMode::OneShot);
        assert!(
            !args.iter().any(|a| a == "--approval-mode"),
            "ei approval-mode kun yolo=false"
        );
        assert!(!args.iter().any(|a| a == "yolo"));
    }

    #[test]
    fn build_args_yolo_enabled_adds_approval_mode() {
        let mut cfg = base_config();
        cfg.yolo = true;
        let args = build_args("x", &cfg, RunMode::OneShot);
        assert_eq!(flag_value(&args, "--approval-mode"), Some("yolo"));
    }

    #[test]
    fn build_args_sandbox_toggles_flag() {
        let mut cfg = base_config();
        assert!(!build_args("x", &cfg, RunMode::OneShot)
            .iter()
            .any(|a| a == "--sandbox"));
        cfg.sandbox = true;
        assert!(build_args("x", &cfg, RunMode::OneShot)
            .iter()
            .any(|a| a == "--sandbox"));
    }

    #[test]
    fn build_args_include_dirs_repeated_per_entry() {
        let mut cfg = base_config();
        cfg.include_dirs = vec![PathBuf::from("src"), PathBuf::from("docs")];
        let args = build_args("x", &cfg, RunMode::OneShot);
        let count = args.iter().filter(|a| *a == "--include-directories").count();
        assert_eq!(count, 2, "yksi lippu per hakemisto");
        assert!(args.iter().any(|a| a == "src"));
        assert!(args.iter().any(|a| a == "docs"));
    }

    #[test]
    fn build_args_policies_repeated_per_entry() {
        let mut cfg = base_config();
        cfg.policies = vec![PathBuf::from("policy1.json"), PathBuf::from("policy2.json")];
        let args = build_args("x", &cfg, RunMode::OneShot);
        let count = args.iter().filter(|a| *a == "--policy").count();
        assert_eq!(count, 2, "yksi lippu per politiikka");
        assert!(args.iter().any(|a| a == "policy1.json"));
        assert!(args.iter().any(|a| a == "policy2.json"));
    }

    #[test]
    fn check_installed_does_not_panic_and_reports_clearly() {
        // Riippumatta siitä onko 'gemini' asennettu testiympäristössä,
        // funktio palauttaa siistin Result-arvon eikä panikoi.
        match check_installed() {
            // Asennettu: versiomerkkijono palautuu (sisältö ympäristöstä riippuva).
            Ok(_version) => {}
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("Gemini CLI"),
                    "virheviesti mainitsee Gemini CLI:n: {msg}"
                );
            }
        }
    }
}

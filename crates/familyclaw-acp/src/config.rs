//! ACP-agentin konfiguraatio.
//!
//! Määrittelee CLI-agentin binääripolun, argumentit ja käyttäytymisen.
//! Kaikki ladataan ajonaikaisesti — ei kovakoodattuja arvoja.

use std::path::PathBuf;

/// ACP-agentin konfiguraatio.
///
/// # Esimerkki
/// ```
/// use familyclaw_acp::AcpAgentConfig;
///
/// let config = AcpAgentConfig::new("claude")
///     .with_permission_mode("bypass_permissions");
/// ```
#[derive(Debug, Clone)]
pub struct AcpAgentConfig {
    /// CLI-binäärin polku (esim. `claude`, `gemini`, `qodercli`).
    pub binary: PathBuf,
    /// Lisäargumentit binäärille (esim. `--model`, `--yolo`).
    pub args: Vec<String>,
    /// Työhakemisto agentille.
    pub working_dir: Option<PathBuf>,
    /// Agentin nimi (esim. "claude", "gemini", "qoder").
    pub name: String,
    /// Aika sekunneissa jonka jälkeen agentti tapetaan jos se ei vastaa.
    pub timeout_secs: u64,
}

impl AcpAgentConfig {
    /// Luo uuden konfiguraation annetulle binäärille.
    ///
    /// `binary` voi olla pelkkä nimi (katsotaan PATH:sta) tai absoluuttinen polku.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        let bin: PathBuf = binary.into();
        let name = bin
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent")
            .to_string();

        Self {
            binary: bin,
            args: Vec::new(),
            working_dir: None,
            name,
            timeout_secs: 120,
        }
    }

    /// Lisää argumentin binäärille.
    #[must_use]
    pub fn with_arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Asettaa käyttöoikeustilan (`default`, `accept_edits`, `bypass_permissions`, `plan`).
    #[must_use]
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.args.push(format!("--permission-mode={}", mode.into()));
        self
    }

    /// Asettaa mallin.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.args.push(format!("--model={}", model.into()));
        self
    }

    /// Asettaa työhakemiston.
    #[must_use]
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Asettaa aikakatkaisun sekunneissa.
    #[must_use]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_derives_name_from_binary_file_stem() {
        let config = AcpAgentConfig::new("agent_a");
        assert_eq!(config.name, "agent_a");
    }

    #[test]
    fn new_strips_extension_from_name() {
        let config = AcpAgentConfig::new("agent_a.exe");
        assert_eq!(config.name, "agent_a");
    }

    #[test]
    fn new_derives_name_from_absolute_path_file_stem() {
        let mut path = std::env::temp_dir();
        path.push("bin");
        path.push("agent_a.exe");
        let config = AcpAgentConfig::new(path.clone());
        assert_eq!(config.name, "agent_a");
        // binary säilyttää koko polun, vain name on file_stem.
        assert_eq!(config.binary, path);
    }

    #[test]
    fn new_falls_back_to_agent_when_no_file_stem() {
        // Tyhjä polku ei tuota file_stemiä → fallback "agent".
        let config = AcpAgentConfig::new("");
        assert_eq!(config.name, "agent");
    }

    #[test]
    fn new_default_timeout_is_120_secs() {
        let config = AcpAgentConfig::new("agent_a");
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn new_defaults_args_empty_and_working_dir_none() {
        let config = AcpAgentConfig::new("agent_a");
        assert!(config.args.is_empty());
        assert!(config.working_dir.is_none());
    }

    #[test]
    fn with_permission_mode_yields_exact_flag() {
        let config = AcpAgentConfig::new("agent_a").with_permission_mode("bypass_permissions");
        assert_eq!(config.args, vec!["--permission-mode=bypass_permissions"]);
    }

    #[test]
    fn with_model_yields_exact_flag() {
        let config = AcpAgentConfig::new("agent_a").with_model("model_x");
        assert_eq!(config.args, vec!["--model=model_x"]);
    }

    #[test]
    fn with_arg_appends_raw_argument() {
        let config = AcpAgentConfig::new("agent_a").with_arg("--yolo");
        assert_eq!(config.args, vec!["--yolo"]);
    }

    #[test]
    fn builder_chain_preserves_arg_accumulation_order() {
        let config = AcpAgentConfig::new("agent_a")
            .with_arg("--first")
            .with_permission_mode("default")
            .with_model("model_x")
            .with_arg("--last");
        assert_eq!(
            config.args,
            vec![
                "--first",
                "--permission-mode=default",
                "--model=model_x",
                "--last",
            ]
        );
    }

    #[test]
    fn with_timeout_overrides_default() {
        let config = AcpAgentConfig::new("agent_a").with_timeout(45);
        assert_eq!(config.timeout_secs, 45);
    }

    #[test]
    fn with_working_dir_sets_some_path() {
        let dir = std::env::temp_dir();
        let config = AcpAgentConfig::new("agent_a").with_working_dir(dir.clone());
        assert_eq!(config.working_dir, Some(dir));
    }
}

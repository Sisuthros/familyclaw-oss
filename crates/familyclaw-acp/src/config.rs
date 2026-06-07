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

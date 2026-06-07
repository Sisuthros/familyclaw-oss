//! ACP-viestityypit — JSON-serialisoitavat request/response-rakenteet.
//!
//! ACP (Agent Communication Protocol) käyttää JSON-RPC 2.0 -tyyppistä
//! viestimuotoa stdin/stdout:n yli. Tämä moduuli määrittelee protokollan
//! kannalta olennaiset tietorakenteet.

use serde::{Deserialize, Serialize};

/// ACP-kutsu agentille.
///
/// Vastaa käyttäjän promptia: "tee tämä asia".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpRequest {
    /// Prompt-teksti agentille.
    pub prompt: String,
    /// Käyttöoikeustila (esim. "default", "bypass_permissions").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Työhakemisto.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl AcpRequest {
    /// Luo uuden ACP-kutsun promptilla.
    #[must_use]
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            permission_mode: None,
            working_dir: None,
        }
    }

    /// Asettaa käyttöoikeustilan.
    #[must_use]
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = Some(mode.into());
        self
    }
}

/// ACP-vastaus agentilta.
///
/// Agentti palauttaa tekstimuotoisen vastauksen promptiin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpResponse {
    /// Agentin tekstivastaus.
    pub content: String,
    /// Työkalukutsut joita agentti teki (tiedostojen luku, shell-komennot jne.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AcpToolCall>,
    /// Kuinka monta tokenia käytettiin (jos agentti raportoi).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<AcpTokenUsage>,
    /// Sessio-ID jatkuvuutta varten.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Agentin tekemä työkalukutsu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpToolCall {
    /// Työkalun nimi (esim. "read_file", "execute_command").
    pub tool: String,
    /// Työkalun argumentit JSON-muodossa.
    pub arguments: serde_json::Value,
    /// Työkalun palauttama tulos (jos saatavissa).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// Työkalun palauttama tulos agentille.
///
/// Lähetetään takaisin agentille kun työkalu on suoritettu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpToolResult {
    /// Työkalukutsun ID johon tämä on vastaus.
    pub tool_call_id: String,
    /// Työkalun tulostama sisältö.
    pub content: String,
    /// Oliko suoritus onnistunut.
    #[serde(default = "default_success")]
    pub success: bool,
}

fn default_success() -> bool {
    true
}

/// Token-käyttötilasto.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AcpTokenUsage {
    /// Syötetokenien määrä.
    pub input_tokens: u32,
    /// Tulostetokenien määrä.
    pub output_tokens: u32,
}

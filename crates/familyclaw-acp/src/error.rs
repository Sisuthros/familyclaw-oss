//! ACP-virhetyypit.
//!
//! Kaikki virheet joita ACP-clientti voi kohdata: spawn, JSON, I/O, timeout.

use std::path::PathBuf;

/// ACP-clientin virhetyypit.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// Binäärin spawn epäonnistui.
    #[error("failed to spawn ACP agent '{binary}': {reason}")]
    Spawn {
        /// Binääri jota yritettiin käynnistää.
        binary: PathBuf,
        /// Syy.
        reason: String,
    },

    /// JSON-serialisointi/epäserialisointi epäonnistui.
    #[error("ACP JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O-virhe stdin/stdout-yhteydessä.
    #[error("ACP I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Agentti aikakatkaistiin.
    #[error("ACP agent timeout after {secs}s: {agent}")]
    Timeout {
        /// Agentin nimi.
        agent: String,
        /// Aikaraja sekunneissa.
        secs: u64,
    },

    /// Agentti palautti virheellisen vastauksen.
    #[error("ACP unexpected response from '{agent}': {detail}")]
    UnexpectedResponse {
        /// Agentin nimi.
        agent: String,
        /// Tarkennus.
        detail: String,
    },

    /// Agentti kaatui (exit code != 0).
    #[error("ACP agent '{agent}' crashed with exit code {code}")]
    Crash {
        /// Agentin nimi.
        agent: String,
        /// Prosessin exit-koodi.
        code: i32,
    },
}

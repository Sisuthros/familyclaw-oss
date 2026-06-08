//! # familyclaw-acp
//!
//! **ACP (Agent Communication Protocol) client** — spawnaa ja ohjaa CLI-agentteja
//! (Claude, Gemini, Qoder) stdio-yhteyden yli.
//!
//! ## Arkkitehtuuri
//!
//! [`AcpClient`] käynnistää CLI-agentin aliprosessina (`claude --acp`,
//! `gemini --acp`, `qodercli --acp`), hallinnoi JSON-viestiliikennettä
//! stdin/stdout:n yli, ja palauttaa vastaukset [`AcpResponse`]-olioina.
//!
//! ## Integraatio `familyclaw-agent`:in
//!
//! [`AcpLlmClient`] implementoi HTTP [`LlmClient`](familyclaw_agent::llm::LlmClient):n
//! kaltaisen rajapinnan, jolloin FamilyClaw voi käyttää CLI-agentteja
//! pudotuskorvaavina LLM-asiakkaina.
//!
//! ## KERROS A (OSS)
//!
//! Ei kovakoodattuja agenttipolkuja, malleja, avaimia tai perheenjäsenten
//! sieluja. Kaikki konfiguroidaan ajonaikaisesti [`AcpAgentConfig`]:n kautta.

pub mod client;
pub mod config;
pub mod error;
pub mod message;

pub use client::AcpClient;
pub use config::AcpAgentConfig;
pub use error::AcpError;
pub use message::{AcpRequest, AcpResponse, AcpToolCall, AcpToolResult};

/// Spawnaa uuden ACP-agentin ja palauttaa aktiivisen clientin.
///
/// # Errors
/// [`AcpError::Spawn`] jos binääriä ei löydy tai prosessin käynnistys
/// epäonnistuu.
pub fn spawn(config: &AcpAgentConfig) -> Result<AcpClient, AcpError> {
    AcpClient::spawn(config)
}

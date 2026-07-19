//! # familyclaw-acp
//!
//! **ACP (Agent Communication Protocol) client** — spawns and drives CLI
//! agents (Claude, Gemini, Qoder) over a stdio connection.
//!
//! ## Architecture
//!
//! [`AcpClient`] launches a CLI agent as a subprocess (`claude --acp`,
//! `gemini --acp`, `qodercli --acp`), manages JSON message traffic over
//! stdin/stdout, and returns responses as [`AcpResponse`] objects.
//!
//! ## Integration with `familyclaw-agent`
//!
//! `AcpLlmClient` implements an interface similar to
//! `familyclaw_agent::llm::LlmClient`'s HTTP client, letting `FamilyClaw`
//! use CLI agents as drop-in replacement LLM clients.
//!
//! ## Layer A (OSS)
//!
//! No hardcoded agent paths, models, keys, or family members' souls.
//! Everything is configured at runtime via [`AcpAgentConfig`].

pub mod client;
pub mod config;
pub mod error;
pub mod message;

pub use client::AcpClient;
pub use config::AcpAgentConfig;
pub use error::AcpError;
pub use message::{AcpRequest, AcpResponse, AcpToolCall, AcpToolResult};

/// Spawns a new ACP agent and returns an active client.
///
/// # Errors
/// [`AcpError::Spawn`] if the binary cannot be found or the process fails
/// to start.
pub fn spawn(config: &AcpAgentConfig) -> Result<AcpClient, AcpError> {
    AcpClient::spawn(config)
}

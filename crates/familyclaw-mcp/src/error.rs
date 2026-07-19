//! Error types for the MCP client (Layer A).

use thiserror::Error;

/// An MCP client error.
#[derive(Debug, Error)]
pub enum McpError {
    /// Parsing an environment variable failed.
    #[error("ympäristön jäsennys: {0}")]
    EnvParse(String),
    /// Spawning a process failed.
    #[error("prosessin käynnistys: {0}")]
    ProcessSpawn(String),
    /// Sending a JSON-RPC message failed.
    #[error("lähetys: {0}")]
    TransportSend(String),
    /// Reading a JSON-RPC response failed.
    #[error("vastaanotto: {0}")]
    TransportRecv(String),
    /// The JSON-RPC response contained an error.
    #[error("json-rpc virhe: {0}")]
    JsonRpc(String),
    /// An expected JSON field was missing or had the wrong type.
    #[error("protokolla: {0}")]
    Protocol(String),
    /// The HTTP transport is not enabled (the `http` feature is off).
    #[error("http-kuljetus ei käytössä (käännä feature http)")]
    HttpDisabled,
    /// Registering a skill with the action runtime failed.
    #[error("skill-rekisteröinti: {0}")]
    SkillRegister(String),
}

/// This crate's [`Result`] alias.
pub type Result<T> = std::result::Result<T, McpError>;

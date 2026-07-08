//! Virhetyypit MCP-asiakkaalle (KERROS A).

use thiserror::Error;

/// MCP-asiakkaan virhe.
#[derive(Debug, Error)]
pub enum McpError {
    /// Ympäristömuuttujan jäsennys epäonnistui.
    #[error("ympäristön jäsennys: {0}")]
    EnvParse(String),
    /// Prosessin käynnistys epäonnistui.
    #[error("prosessin käynnistys: {0}")]
    ProcessSpawn(String),
    /// JSON-RPC-viestin lähetys epäonnistui.
    #[error("lähetys: {0}")]
    TransportSend(String),
    /// JSON-RPC-vastauksen lukeminen epäonnistui.
    #[error("vastaanotto: {0}")]
    TransportRecv(String),
    /// JSON-RPC-vastaus sisälsi virheen.
    #[error("json-rpc virhe: {0}")]
    JsonRpc(String),
    /// Odotettu JSON-kenttä puuttui tai oli väärää tyyppiä.
    #[error("protokolla: {0}")]
    Protocol(String),
    /// HTTP-kuljetus ei ole käytössä (feature `http` pois).
    #[error("http-kuljetus ei käytössä (käännä feature http)")]
    HttpDisabled,
    /// Toimintoajoympäristön rekisteröinti epäonnistui.
    #[error("skill-rekisteröinti: {0}")]
    SkillRegister(String),
}

/// Crate-kohtainen [`Result`].
pub type Result<T> = std::result::Result<T, McpError>;

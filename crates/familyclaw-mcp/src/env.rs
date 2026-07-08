//! `FAMILYCLAW_MCP_SERVERS` -ympäristömuuttujan jäsennys (KERROS A).
//!
//! Muoto: `name=command args` (stdio) tai `name=http://host[:port][/path]`.
//! Useita palvelimia erotetaan puolipisteellä (`;`).

use crate::error::{McpError, Result};

/// Yhden MCP-palvelimen konfiguraatio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Looginen nimi (prefiksi taidoille).
    pub name: String,
    /// Kuljetustyyppi.
    pub transport: McpTransportConfig,
}

/// Stdio- tai HTTP-kuljetus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransportConfig {
    /// Käynnistä prosessi ja käytä stdin/stdout JSON-RPC -rivejä.
    Stdio {
        /// Suoritettava komento (ensimmäinen argumentti).
        command: String,
        /// Lisäargumentit.
        args: Vec<String>,
    },
    /// HTTP POST JSON-RPC `/mcp`-päätepisteeseen.
    Http {
        /// Täysi tai juuri-URL (polku täydennetään tarvittaessa).
        url: String,
    },
}

/// Jäsentää `FAMILYCLAW_MCP_SERVERS`-arvon.
///
/// # Errors
/// Palauttaa [`McpError::EnvParse`] jos merkkijono on tyhjä segmentin jälkeen
/// tai `name=value` -pari on epäkelpo.
pub fn parse_mcp_servers(raw: &str) -> Result<Vec<McpServerConfig>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for segment in trimmed.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        out.push(parse_single_server(segment)?);
    }
    Ok(out)
}

/// Lukee `FAMILYCLAW_MCP_SERVERS` prosessin ympäristöstä.
pub fn load_mcp_servers_from_env() -> Result<Vec<McpServerConfig>> {
    match std::env::var("FAMILYCLAW_MCP_SERVERS") {
        Ok(raw) => parse_mcp_servers(&raw),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(e) => Err(McpError::EnvParse(format!("FAMILYCLAW_MCP_SERVERS: {e}"))),
    }
}

fn parse_single_server(segment: &str) -> Result<McpServerConfig> {
    let (name, value) = segment
        .split_once('=')
        .ok_or_else(|| McpError::EnvParse(format!("odotettiin name=value, saatiin: {segment}")))?;

    let name = name.trim();
    if name.is_empty() {
        return Err(McpError::EnvParse("palvelimen nimi on tyhjä".to_string()));
    }

    let value = value.trim();
    if value.is_empty() {
        return Err(McpError::EnvParse(format!(
            "palvelimelle '{name}' ei annettu komentoa tai URLia"
        )));
    }

    let transport = if value.starts_with("http://") || value.starts_with("https://") {
        McpTransportConfig::Http {
            url: normalize_http_url(value),
        }
    } else {
        let mut parts = value.split_whitespace().map(str::to_string);
        let command = parts.next().ok_or_else(|| {
            McpError::EnvParse(format!("stdio-komento puuttuu palvelimelle '{name}'"))
        })?;
        let args: Vec<String> = parts.collect();
        McpTransportConfig::Stdio { command, args }
    };

    Ok(McpServerConfig {
        name: name.to_string(),
        transport,
    })
}

fn normalize_http_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/mcp") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/mcp")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_server() {
        let configs = parse_mcp_servers("mock=mock-mcp-stdio-server").expect("parse");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "mock");
        assert_eq!(
            configs[0].transport,
            McpTransportConfig::Stdio {
                command: "mock-mcp-stdio-server".to_string(),
                args: vec![],
            }
        );
    }

    #[test]
    fn parses_stdio_with_args() {
        let configs = parse_mcp_servers("agent_a=python server.py --port 9").expect("parse");
        assert_eq!(configs[0].name, "agent_a");
        assert!(matches!(
            &configs[0].transport,
            McpTransportConfig::Stdio { command, args }
            if command == "python" && args == &["server.py", "--port", "9"]
        ));
    }

    #[test]
    fn parses_http_and_appends_mcp_path() {
        let configs = parse_mcp_servers("remote=http://127.0.0.1:8080").expect("parse");
        assert!(matches!(
            &configs[0].transport,
            McpTransportConfig::Http { url } if url == "http://127.0.0.1:8080/mcp"
        ));
    }

    #[test]
    fn parses_multiple_servers() {
        let raw = "a=cmd one;b=http://example.com/mcp";
        let configs = parse_mcp_servers(raw).expect("parse");
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].name, "a");
        assert_eq!(configs[1].name, "b");
    }

    #[test]
    fn empty_env_segment_list_is_empty() {
        assert!(parse_mcp_servers("").expect("parse").is_empty());
    }
}

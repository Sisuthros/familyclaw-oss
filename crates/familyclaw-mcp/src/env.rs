//! Parsing of the `FAMILYCLAW_MCP_SERVERS` environment variable (Layer A).
//!
//! Format: `name=command args` (stdio) or `name=http://host[:port][/path]`.
//! Multiple servers are separated by a semicolon (`;`).

use crate::error::{McpError, Result};

/// How much an operator trusts an attached MCP server, and therefore what
/// [`familyclaw_actions::policy::ActionRisk`] / [`familyclaw_actions::policy::ApprovalPolicy`]
/// class its tools are bridged in as.
///
/// This is an **operator declaration**, not something derived from the MCP
/// server's own claims — the bridge cannot inspect what a remote tool
/// actually does, so trust is a per-server classification the operator opts
/// into deliberately (see `docs/MCP_WORKS_WITH.md`).
///
/// - [`McpServerTrust::ReadOnly`] (default): tools are registered as
///   read-only, auto-runnable, no side effects assumed. Safe default for any
///   server the operator has not explicitly reviewed.
/// - [`McpServerTrust::Trusted`]: tools are registered as local-write class
///   (still fail-closed for anything beyond a local write — network/external
///   side effects still require approval). Only use for servers the operator
///   has reviewed and is willing to let run without a per-call approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerTrust {
    /// Safe default: read-only, auto-runnable, no assumed side effects.
    #[default]
    ReadOnly,
    /// Operator-reviewed: local-write class, still fail-closed beyond that.
    Trusted,
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Logical name (prefix for skills).
    pub name: String,
    /// Transport type.
    pub transport: McpTransportConfig,
    /// Operator-declared trust class (defaults to [`McpServerTrust::ReadOnly`]).
    pub trust: McpServerTrust,
}

/// Stdio or HTTP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTransportConfig {
    /// Spawn a process and use JSON-RPC lines over stdin/stdout.
    Stdio {
        /// The command to run (first argument).
        command: String,
        /// Additional arguments.
        args: Vec<String>,
    },
    /// HTTP POST JSON-RPC to the `/mcp` endpoint.
    Http {
        /// Full or root URL (the path is appended if needed).
        url: String,
    },
}

/// Parses the value of `FAMILYCLAW_MCP_SERVERS`.
///
/// # Errors
/// Returns [`McpError::EnvParse`] if a segment is empty after trimming, or
/// if a `name=value` pair is malformed.
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

/// Reads `FAMILYCLAW_MCP_SERVERS` from the process environment.
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
        // The `FAMILYCLAW_MCP_SERVERS` env grammar has no room for a trust
        // marker without breaking the existing `name=value` format — servers
        // attached this way always get the safe ReadOnly default. Trust
        // elevation is a deliberate TOML-config-only opt-in (see `config.rs`
        // / `docs/MCP_WORKS_WITH.md`).
        trust: McpServerTrust::ReadOnly,
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

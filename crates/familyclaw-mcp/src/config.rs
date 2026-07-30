//! First-class, config-driven MCP server attachment (Layer A).
//!
//! This is the **primary** "attach my existing MCP servers as trusted,
//! runnable [`ActionRuntime`](familyclaw_actions::facade::ActionRuntime)
//! skills" path: a small, documented TOML schema loaded from a file (path
//! given by `FAMILYCLAW_MCP_CONFIG`), distinct from the
//! `familyclaw import` **quarantine** path (`familyclaw-agent/src/import_cli.rs`).
//!
//! ## Why a separate path from `import`
//!
//! `familyclaw import` converts an *export* from another agent runtime
//! (`OpenClaw`/Hermes) into `FamilyClaw`'s representation, and is deliberately
//! **lossy and paranoid**: imported skills are quarantined
//! ([`ActionRisk::ExecuteCode`] + [`ApprovalPolicy::AlwaysRequireApproval`],
//! never registered, never executed) because the importer cannot verify what
//! the source skill's code actually does.
//!
//! This module is the opposite case: the operator already has a **live,
//! running MCP server** — a real protocol the bridge can `tools/list` and
//! `tools/call` against without guessing. There is no code to sandbox; the
//! MCP protocol itself is the contract. So instead of quarantining, this path
//! **registers real, runnable skills directly**, with a risk/approval class
//! the operator chooses explicitly per server ([`McpServerTrust`]) — never
//! auto-elevated, always fail-closed to [`McpServerTrust::ReadOnly`] unless
//! the operator opts a server into [`McpServerTrust::Trusted`] in the config
//! file.
//!
//! ## TOML schema
//!
//! ```toml
//! # familyclaw-mcp.toml — attach existing MCP servers as ActionRuntime skills.
//!
//! [[servers]]
//! name = "docs_search"          # logical name; prefixes bridged skill ids
//! command = "npx"               # stdio transport: command + args
//! args = ["-y", "@my/mcp-docs-server"]
//! trust = "read_only"           # default; omit for the same effect
//!
//! [[servers]]
//! name = "local_notes"
//! command = "my-notes-mcp-server"
//! trust = "trusted"             # operator has reviewed this server
//!
//! [[servers]]
//! name = "remote_kb"
//! url = "https://kb.internal.example/mcp"   # HTTP transport
//! trust = "read_only"
//! ```
//!
//! Each entry needs exactly one of `command` (stdio) or `url` (HTTP). `args`
//! is optional (stdio only, defaults to none). `trust` is optional and
//! defaults to `"read_only"`.
//!
//! # Errors
//! [`McpError::EnvParse`] for a malformed file (bad TOML, missing name,
//! neither/both of `command`/`url` set, unknown `trust` value).

use std::path::Path;

use serde::Deserialize;

use crate::env::{McpServerConfig, McpServerTrust, McpTransportConfig};
use crate::error::{McpError, Result};

/// The whole config file: a flat list of servers to attach.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpFileConfig {
    /// Servers to attach as bridged skills.
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

/// A single server entry in the TOML config file.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerEntry {
    /// Logical name (prefix for bridged skills).
    pub name: String,
    /// Stdio transport: the command to run. Mutually exclusive with `url`.
    #[serde(default)]
    pub command: Option<String>,
    /// Stdio transport: additional arguments (only meaningful with `command`).
    #[serde(default)]
    pub args: Vec<String>,
    /// HTTP transport: the server's URL. Mutually exclusive with `command`.
    #[serde(default)]
    pub url: Option<String>,
    /// Operator-declared trust class. Defaults to [`McpServerTrust::ReadOnly`].
    #[serde(default)]
    pub trust: McpServerTrust,
}

impl McpServerEntry {
    /// Validates and converts this entry into a runnable [`McpServerConfig`].
    ///
    /// # Errors
    /// [`McpError::EnvParse`] if the name is empty, or the entry has neither
    /// / both of `command` and `url` set.
    pub fn into_server_config(self) -> Result<McpServerConfig> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(McpError::EnvParse(
                "mcp config: server entry has an empty name".to_string(),
            ));
        }

        let transport = match (self.command, self.url) {
            (Some(command), None) => {
                let command = command.trim().to_string();
                if command.is_empty() {
                    return Err(McpError::EnvParse(format!(
                        "mcp config: server '{name}' has an empty command"
                    )));
                }
                McpTransportConfig::Stdio {
                    command,
                    args: self.args,
                }
            }
            (None, Some(url)) => {
                let url = url.trim();
                if url.is_empty() {
                    return Err(McpError::EnvParse(format!(
                        "mcp config: server '{name}' has an empty url"
                    )));
                }
                McpTransportConfig::Http {
                    url: normalize_http_url(url),
                }
            }
            (None, None) => {
                return Err(McpError::EnvParse(format!(
                "mcp config: server '{name}' needs exactly one of `command` or `url` (got neither)"
            )))
            }
            (Some(_), Some(_)) => {
                return Err(McpError::EnvParse(format!(
                    "mcp config: server '{name}' needs exactly one of `command` or `url` (got both)"
                )))
            }
        };

        Ok(McpServerConfig {
            name,
            transport,
            trust: self.trust,
        })
    }
}

fn normalize_http_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/mcp") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/mcp")
    }
}

/// Parses a TOML config document's raw text into runnable [`McpServerConfig`]s.
///
/// # Errors
/// [`McpError::EnvParse`] if the TOML is malformed or any entry is invalid
/// (see [`McpServerEntry::into_server_config`]). Duplicate names are
/// rejected — an operator typo should fail loudly, not silently shadow a
/// server.
pub fn parse_mcp_config(raw: &str) -> Result<Vec<McpServerConfig>> {
    let file: McpFileConfig =
        toml::from_str(raw).map_err(|e| McpError::EnvParse(format!("mcp config toml: {e}")))?;

    let mut out = Vec::with_capacity(file.servers.len());
    for entry in file.servers {
        let config = entry.into_server_config()?;
        if out.iter().any(|c: &McpServerConfig| c.name == config.name) {
            return Err(McpError::EnvParse(format!(
                "mcp config: duplicate server name '{}'",
                config.name
            )));
        }
        out.push(config);
    }
    Ok(out)
}

/// Loads and parses the TOML config file at `path`.
///
/// # Errors
/// [`McpError::EnvParse`] if the file cannot be read or its contents are
/// invalid (see [`parse_mcp_config`]).
pub fn load_mcp_config_file(path: impl AsRef<Path>) -> Result<Vec<McpServerConfig>> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|e| {
        McpError::EnvParse(format!("mcp config: reading '{}': {e}", path.display()))
    })?;
    parse_mcp_config(&raw)
}

/// Reads `FAMILYCLAW_MCP_CONFIG` (a path to a TOML config file) and loads it
/// if set. Unset → empty list (this source is fully optional; the env-var
/// quick-attach path in [`crate::env`] keeps working on its own).
///
/// # Errors
/// [`McpError::EnvParse`] if the env var is set but the file cannot be read
/// or parsed.
pub fn load_mcp_config_from_env() -> Result<Vec<McpServerConfig>> {
    match std::env::var("FAMILYCLAW_MCP_CONFIG") {
        Ok(path) => load_mcp_config_file(path),
        Err(std::env::VarError::NotPresent) => Ok(Vec::new()),
        Err(e) => Err(McpError::EnvParse(format!("FAMILYCLAW_MCP_CONFIG: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stdio_and_http_entries_with_trust() {
        let toml = r#"
[[servers]]
name = "docs_search"
command = "npx"
args = ["-y", "@my/mcp-docs-server"]

[[servers]]
name = "local_notes"
command = "my-notes-mcp-server"
trust = "trusted"

[[servers]]
name = "remote_kb"
url = "https://kb.internal.example"
trust = "read_only"
"#;
        let configs = parse_mcp_config(toml).expect("parse");
        assert_eq!(configs.len(), 3);

        assert_eq!(configs[0].name, "docs_search");
        assert_eq!(configs[0].trust, McpServerTrust::ReadOnly, "default trust");
        assert!(matches!(
            &configs[0].transport,
            McpTransportConfig::Stdio { command, args }
            if command == "npx" && args == &["-y", "@my/mcp-docs-server"]
        ));

        assert_eq!(configs[1].name, "local_notes");
        assert_eq!(configs[1].trust, McpServerTrust::Trusted);

        assert_eq!(configs[2].name, "remote_kb");
        assert!(matches!(
            &configs[2].transport,
            McpTransportConfig::Http { url } if url == "https://kb.internal.example/mcp"
        ));
    }

    #[test]
    fn empty_file_is_empty_list() {
        assert!(parse_mcp_config("").expect("parse").is_empty());
        assert!(parse_mcp_config("servers = []").expect("parse").is_empty());
    }

    #[test]
    fn missing_name_is_rejected() {
        let err = parse_mcp_config(r#"[[servers]] command = "x""#).expect_err("must fail");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn missing_transport_is_rejected() {
        let err =
            parse_mcp_config(r#"[[servers]] name = "a""#).expect_err("neither command nor url");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn both_command_and_url_is_rejected() {
        let toml = r#"[[servers]]
name = "a"
command = "x"
url = "https://example.com"
"#;
        let err = parse_mcp_config(toml).expect_err("both set");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let toml = r#"
[[servers]]
name = "dup"
command = "a"

[[servers]]
name = "dup"
command = "b"
"#;
        let err = parse_mcp_config(toml).expect_err("duplicate name");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn unknown_trust_value_is_rejected() {
        let toml = r#"[[servers]]
name = "a"
command = "x"
trust = "yolo"
"#;
        let err = parse_mcp_config(toml).expect_err("unknown trust");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn malformed_toml_is_rejected() {
        let err = parse_mcp_config("not [ valid toml").expect_err("malformed");
        assert!(matches!(err, McpError::EnvParse(_)));
    }

    #[test]
    fn load_mcp_config_file_reads_from_disk() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-mcp-config-test-{}.toml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            r#"[[servers]]
name = "from_disk"
command = "echo"
"#,
        )
        .expect("write temp config");

        let configs = load_mcp_config_file(&path).expect("load");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "from_disk");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_mcp_config_file_missing_is_env_parse_error() {
        let err = load_mcp_config_file("this/path/does/not/exist-xyz.toml")
            .expect_err("missing file must fail");
        assert!(matches!(err, McpError::EnvParse(_)));
    }
}

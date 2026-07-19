//! # familyclaw-mcp
//!
//! An MCP client for the `FamilyClaw` platform (Layer A, OSS): stdio and
//! HTTP transport, tool listing/invocation, and a bridge to
//! [`ActionRuntime`] skills.

pub mod bridge;
pub mod client;
pub mod env;
pub mod error;
pub mod redact;
pub mod transport;

pub use bridge::{register_from_env, register_mcp_skills};
pub use client::McpClient;
pub use env::{load_mcp_servers_from_env, McpServerConfig, McpTransportConfig};
pub use error::{McpError, Result};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }
}

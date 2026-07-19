//! MCP client: tool listing and invocation (Layer A).

use std::sync::Arc;

use familyclaw_actions::mcp::{McpToolDescriptor, McpToolResult};
use familyclaw_actions::policy::SkillPermission;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::env::{McpServerConfig, McpTransportConfig};
use crate::error::{McpError, Result};
use crate::redact::redact_for_log;
use crate::transport::Transport;

/// An MCP server client (stdio or HTTP).
pub struct McpClient {
    server_name: String,
    transport: Mutex<Transport>,
}

impl McpClient {
    /// Connects to the server and performs the MCP handshake.
    ///
    /// # Errors
    /// Returns an error if opening the transport or the `initialize`
    /// handshake fails.
    pub async fn connect(config: McpServerConfig) -> Result<SharedMcpClient> {
        let server_name = config.name.clone();
        let mut transport = Transport::connect(&config.transport, &server_name)?;
        transport.handshake().await?;
        Ok(Arc::new(Self {
            server_name,
            transport: Mutex::new(transport),
        }))
    }

    /// The server's logical name.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Whether the configuration is for an HTTP transport.
    #[must_use]
    pub fn is_http(config: &McpTransportConfig) -> bool {
        matches!(config, McpTransportConfig::Http { .. })
    }

    /// Lists the server's tools via the MCP protocol.
    ///
    /// # Errors
    /// Returns an error if the JSON-RPC call or parsing the protocol
    /// response fails.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        let mut transport = self.transport.lock().await;
        let result = transport.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Protocol("tools/list missing tools array".to_string()))?;

        let mut descriptors = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| McpError::Protocol("tool missing name".to_string()))?;
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("MCP tool")
                .to_string();
            let input_schema = tool
                .get("inputSchema")
                .or_else(|| tool.get("input_schema"))
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object" }));
            if !input_schema.is_object() {
                return Err(McpError::Protocol(format!(
                    "tool '{name}' input schema must be object"
                )));
            }

            descriptors.push(McpToolDescriptor::new(
                qualify_tool_name(&self.server_name, name),
                description,
                input_schema,
                SkillPermission::NetworkRead,
            ));
        }
        Ok(descriptors)
    }

    /// Calls the named MCP tool.
    ///
    /// `tool_name` may be fully qualified (`mcp_server_tool`) or just the
    /// original tool name — both are accepted.
    ///
    /// # Errors
    /// Returns an error if the JSON-RPC call, the protocol response, or the
    /// tool's `isError` flag indicates failure.
    pub async fn call_tool(&self, tool_name: &str, input: Value) -> Result<McpToolResult> {
        let remote_name = remote_tool_name(&self.server_name, tool_name);
        tracing::debug!(
            target: "familyclaw::mcp",
            server = %self.server_name,
            tool = %remote_name,
            "calling MCP tool"
        );

        let params = json!({
            "name": remote_name,
            "arguments": input,
        });
        let mut transport = self.transport.lock().await;
        let result = transport.request("tools/call", params).await?;

        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let detail = result.get("content").map_or_else(
                || "tool returned isError".to_string(),
                |c| redact_for_log(&c.to_string()),
            );
            return Err(McpError::JsonRpc(format!("tool error: {detail}")));
        }

        let output = extract_tool_output(&result);
        Ok(McpToolResult::untrusted(output))
    }
}

/// A shared [`Arc`] client for use by dynamic skills.
pub type SharedMcpClient = Arc<McpClient>;

fn qualify_tool_name(server: &str, tool: &str) -> String {
    format!("mcp_{server}_{tool}")
}

fn remote_tool_name(server: &str, tool: &str) -> String {
    let prefix = format!("mcp_{server}_");
    tool.strip_prefix(&prefix).unwrap_or(tool).to_string()
}

fn extract_tool_output(result: &Value) -> Value {
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|block| {
                block
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|t| *t == "text")
                    .and_then(|_| block.get("text").and_then(Value::as_str))
            })
            .collect();
        if !texts.is_empty() {
            return json!({ "text": texts.join("\n") });
        }
    }
    if result.get("structuredContent").is_some() {
        return result
            .get("structuredContent")
            .cloned()
            .unwrap_or(json!({}));
    }
    result.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualify_and_remote_roundtrip() {
        let q = qualify_tool_name("mock", "echo");
        assert_eq!(q, "mcp_mock_echo");
        assert_eq!(remote_tool_name("mock", &q), "echo");
        assert_eq!(remote_tool_name("mock", "echo"), "echo");
    }

    #[test]
    fn extract_text_content() {
        let result = json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" }
            ]
        });
        let out = extract_tool_output(&result);
        assert_eq!(out, json!({ "text": "hello\nworld" }));
    }
}

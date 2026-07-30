//! Stdio MCP integration tests using the mock server.

use familyclaw_actions::facade::ActionRuntime;
use familyclaw_actions::policy::ActionRisk;
use familyclaw_mcp::{
    register_mcp_skills, McpClient, McpServerConfig, McpServerTrust, McpTransportConfig,
};
use serde_json::json;

fn mock_server_command() -> String {
    std::env::var("CARGO_BIN_EXE_mock-mcp-stdio-server")
        .expect("mock-mcp-stdio-server binary must be built for integration tests")
}

#[tokio::test]
async fn stdio_client_lists_and_calls_echo_tool() {
    let config = McpServerConfig {
        name: "mock".to_string(),
        transport: McpTransportConfig::Stdio {
            command: mock_server_command(),
            args: vec![],
        },
        trust: McpServerTrust::ReadOnly,
    };

    let client = McpClient::connect(config).await.expect("connect");
    let tools = client.list_tools().await.expect("list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "mcp_mock_echo");

    let result = client
        .call_tool("mcp_mock_echo", json!({ "user": "agent_a", "msg": "hi" }))
        .await
        .expect("call");
    assert!(result.untrusted);
    assert!(result.output.get("text").is_some());
}

#[tokio::test]
async fn register_mcp_skills_exposes_tool_on_runtime() {
    let config = McpServerConfig {
        name: "mock".to_string(),
        transport: McpTransportConfig::Stdio {
            command: mock_server_command(),
            args: vec![],
        },
        trust: McpServerTrust::ReadOnly,
    };

    let client = McpClient::connect(config).await.expect("connect");
    let mut runtime = ActionRuntime::new();
    register_mcp_skills(&mut runtime, &client)
        .await
        .expect("register");

    let tool_names: Vec<String> = runtime
        .tool_definitions()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(tool_names.iter().any(|n| n == "mcp_mock_echo"));
}

/// A [`McpServerTrust::Trusted`] server bridges its tools with a
/// [`familyclaw_actions::policy::ActionRisk::WriteLocal`] /
/// `RequireApproval` class instead of the `ReadOnly` default — the
/// config-driven trust elevation actually changes what gets registered.
#[tokio::test]
async fn trusted_server_registers_write_local_risk_skills() {
    let config = McpServerConfig {
        name: "mock".to_string(),
        transport: McpTransportConfig::Stdio {
            command: mock_server_command(),
            args: vec![],
        },
        trust: McpServerTrust::Trusted,
    };

    let client = McpClient::connect(config).await.expect("connect");
    let mut runtime = ActionRuntime::new();
    register_mcp_skills(&mut runtime, &client)
        .await
        .expect("register");

    let skill = runtime
        .list_skills()
        .into_iter()
        .find(|s| s.name == "mcp_mock_echo")
        .expect("skill registered");
    assert_eq!(
        skill.risk,
        ActionRisk::WriteLocal,
        "trusted server -> WriteLocal risk class"
    );
    assert!(
        !skill.requires_approval,
        "WriteLocal + RequireApproval policy still auto-runs (only external/irreversible/... actions require approval)"
    );
}

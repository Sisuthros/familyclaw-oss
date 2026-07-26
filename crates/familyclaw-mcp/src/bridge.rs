//! Bridge from MCP tools to [`ActionRuntime`] skills (Layer A).

use std::sync::Arc;

use async_trait::async_trait;
use familyclaw_actions::facade::ActionRuntime;
use familyclaw_actions::manifest::{default_input_schema, SkillManifest};
use familyclaw_actions::mcp::McpToolDescriptor;
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::skills::Skill;
use familyclaw_actions::{
    ActionError, ActionExecutor, ActionRequest, ActionResult, Result as ActionResult_, SkillId,
};
use uuid::Uuid;

use crate::client::{McpClient, SharedMcpClient};
use crate::config::load_mcp_config_from_env;
use crate::env::{load_mcp_servers_from_env, McpServerTrust};
use crate::error::{McpError, Result};

/// Deterministic UUID v5 namespace for MCP skills.
const MCP_SKILL_NAMESPACE: Uuid = uuid::uuid!("c3d4e5f6-a7b8-4901-c2d3-e4f5a6b7c8d9");

/// Registers all of an MCP client's tools as dynamic [`Skill`]s.
///
/// Each tool gets a manifest derived from its MCP description, and at
/// execution time calls into [`McpClient::call_tool`]. Output is always
/// treated as untrusted (tainted).
///
/// # Errors
/// Returns an error if listing the tools or registering a skill fails.
pub async fn register_mcp_skills(
    runtime: &mut ActionRuntime,
    client: &SharedMcpClient,
) -> Result<()> {
    let tools = client.list_tools().await?;
    for tool in tools {
        let skill = McpDynamicSkill::new(Arc::clone(client), tool);
        runtime
            .register_skill(skill)
            .map_err(|e| McpError::SkillRegister(e.to_string()))?;
    }
    Ok(())
}

/// Reads `FAMILYCLAW_MCP_SERVERS` **and** an optional `FAMILYCLAW_MCP_CONFIG`
/// TOML file, and registers all discovered servers.
///
/// This is the single, first-class "attach my existing MCP servers"
/// entrypoint (config-driven, distinct from the `familyclaw import`
/// quarantine path — see `docs/MCP_WORKS_WITH.md`):
///
/// - `FAMILYCLAW_MCP_SERVERS` (env, `name=command args` / `name=http://…`,
///   semicolon-separated) — quick attach, always [`McpServerTrust::ReadOnly`].
/// - `FAMILYCLAW_MCP_CONFIG` (path to a TOML file, `[[servers]]` — see
///   `crate::config`) — first-class config file, supports per-server
///   [`McpServerTrust`] elevation.
///
/// A server name present in both sources is only connected once — the TOML
/// config entry wins (so an operator can override an env-attached server's
/// trust by giving it an explicit TOML entry with the same name).
///
/// # Errors
/// Returns an error if parsing either source, connecting, or registering a
/// skill fails.
pub async fn register_from_env(runtime: &mut ActionRuntime) -> Result<()> {
    let mut configs = load_mcp_servers_from_env()?;
    let file_configs = load_mcp_config_from_env()?;

    // TOML entries take precedence over an env entry with the same name.
    configs.retain(|c| !file_configs.iter().any(|fc| fc.name == c.name));
    configs.extend(file_configs);

    for config in configs {
        let client = McpClient::connect(config).await?;
        register_mcp_skills(runtime, &client).await?;
    }
    Ok(())
}

/// A dynamic skill that delegates execution to the MCP client.
struct McpDynamicSkill {
    client: SharedMcpClient,
    descriptor: McpToolDescriptor,
    skill_id: SkillId,
}

impl McpDynamicSkill {
    fn new(client: SharedMcpClient, descriptor: McpToolDescriptor) -> Self {
        let seed = format!("{}:{}", client.server_name(), descriptor.name);
        let skill_id = SkillId::from_uuid(Uuid::new_v5(&MCP_SKILL_NAMESPACE, seed.as_bytes()));
        Self {
            client,
            descriptor,
            skill_id,
        }
    }

    fn manifest_from_descriptor(&self) -> SkillManifest {
        let input_schema = if self.descriptor.input_schema.is_object() {
            self.descriptor.input_schema.clone()
        } else {
            default_input_schema()
        };

        // Trust classification is an operator declaration on the *server*
        // (see `McpServerTrust`), not something derived from the tool
        // output — every tool from that server gets the same class.
        let (permissions, risk, approval_policy) = match self.client.server_trust() {
            McpServerTrust::ReadOnly => (
                vec![SkillPermission::NetworkRead],
                ActionRisk::ReadOnly,
                ApprovalPolicy::AutoIfReadOnly,
            ),
            McpServerTrust::Trusted => (
                vec![SkillPermission::NetworkRead, SkillPermission::WriteLocalFiles],
                ActionRisk::WriteLocal,
                ApprovalPolicy::RequireApproval,
            ),
        };

        SkillManifest {
            id: self.skill_id,
            name: self.descriptor.name.clone(),
            version: "1.0.0".to_string(),
            description: self.descriptor.description.clone(),
            permissions,
            risk,
            approval_policy,
            input_hint: None,
            output_hint: None,
            input_schema,
            publisher: None,
            signature: None,
        }
    }
}

impl Skill for McpDynamicSkill {
    fn manifest(&self) -> SkillManifest {
        self.manifest_from_descriptor()
    }
}

#[async_trait]
impl ActionExecutor for McpDynamicSkill {
    async fn execute(&self, request: ActionRequest) -> ActionResult_<ActionResult> {
        let mcp_result = self
            .client
            .call_tool(&self.descriptor.name, request.payload)
            .await
            .map_err(|e| ActionError::ExecutionFailed(e.to_string()))?;

        Ok(ActionResult::success(
            format!("mcp tool '{}' completed", self.descriptor.name),
            mcp_result.output,
            request.now,
        )
        .propagate_input_taint(request.input_untrusted))
    }
}

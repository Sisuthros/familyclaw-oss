//! Silta MCP-työkaluista [`ActionRuntime`]-taidoiksi (KERROS A).

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
use crate::env::load_mcp_servers_from_env;
use crate::error::{McpError, Result};

/// MCP-skillien deterministinen UUID-v5-nimiavaruus.
const MCP_SKILL_NAMESPACE: Uuid = uuid::uuid!("c3d4e5f6-a7b8-4901-c2d3-e4f5a6b7c8d9");

/// Rekisteröi kaikki MCP-asiakkaan työkalut dynaamisina [`Skill`]-taitoina.
///
/// Jokainen työkalu saa manifestin MCP-kuvauksesta ja suorituksessa kutsuu
/// [`McpClient::call_tool`]:ia. Tuloste on aina epäluotettava (taint).
///
/// # Errors
/// Työkalujen listaus tai rekisteröinti epäonnistuu.
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

/// Lukee `FAMILYCLAW_MCP_SERVERS` ja rekisteröi kaikki löydetyt palvelimet.
///
/// # Errors
/// Ympäristön jäsennys, yhteys tai rekisteröinti epäonnistuu.
pub async fn register_from_env(runtime: &mut ActionRuntime) -> Result<()> {
    let configs = load_mcp_servers_from_env()?;
    for config in configs {
        let client = McpClient::connect(config).await?;
        register_mcp_skills(runtime, &client).await?;
    }
    Ok(())
}

/// Dynaaminen taito joka delegoi suorituksen MCP-asiakkaalle.
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

        SkillManifest {
            id: self.skill_id,
            name: self.descriptor.name.clone(),
            version: "1.0.0".to_string(),
            description: self.descriptor.description.clone(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
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

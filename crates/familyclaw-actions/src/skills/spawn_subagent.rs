//! Temporary helper subagent over the resonance bus (Layer A).
//!
//! [`SpawnSubagentSkill`] delegates a task to a [`SubagentSpawner`]
//! implementation, which the runtime wires up to the shared bus. The result
//! is returned to the parent agent via the tool loop.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

const SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");

/// Input: task description and optional helper agent name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSubagentInput {
    /// The task delegated to the helper agent.
    pub task: String,
    /// Optional helper agent name (generic, e.g. `helper_agent`).
    #[serde(default)]
    pub helper_name: Option<String>,
}

/// Interface for a temporary helper agent. Implemented in the runtime layer
/// (no cyclic dependency on `familyclaw-agent`).
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    /// Runs the task with the helper agent and returns the response.
    async fn spawn_and_run(
        &self,
        task: &str,
        helper_name: Option<&str>,
    ) -> std::result::Result<String, String>;
}

/// Skill: delegates a task to a temporary helper agent over the bus.
#[derive(Clone)]
pub struct SpawnSubagentSkill {
    spawner: Option<std::sync::Arc<dyn SubagentSpawner>>,
}

impl SpawnSubagentSkill {
    /// Creates the skill with the given helper agent spawner.
    #[must_use]
    pub fn new(spawner: std::sync::Arc<dyn SubagentSpawner>) -> Self {
        Self {
            spawner: Some(spawner),
        }
    }

    /// Fail-closed: no spawner → the skill registers but rejects calls.
    #[must_use]
    pub fn disabled() -> Self {
        Self { spawner: None }
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }
}

#[async_trait]
impl ActionExecutor for SpawnSubagentSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: SpawnSubagentInput = match serde_json::from_value(request.payload.clone()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid spawn_subagent input: {e}"),
                    request.now,
                ));
            }
        };
        if input.task.trim().is_empty() {
            return Ok(ActionResult::failure(
                "spawn_subagent: task must not be empty",
                request.now,
            ));
        }
        let Some(spawner) = self.spawner.as_ref() else {
            return Ok(ActionResult::failure(
                "spawn_subagent: spawner not configured",
                request.now,
            ));
        };
        match spawner
            .spawn_and_run(input.task.trim(), input.helper_name.as_deref())
            .await
        {
            Ok(result) => {
                let output = json!({ "result": result, "helper": input.helper_name });
                Ok(ActionResult::success(
                    format!("subagent completed ({} chars)", result.chars().count()),
                    output,
                    request.now,
                ))
            }
            Err(e) => Ok(ActionResult::failure(
                format!("spawn_subagent failed: {e}"),
                request.now,
            )),
        }
    }
}

impl Skill for SpawnSubagentSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "spawn_subagent".to_string(),
            version: "1.0.0".to_string(),
            description: "Delegoi tehtävän väliaikaiselle apuagentille resonance-busilla."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ task, helper_name? }".to_string()),
            output_hint: Some("{ result }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "helper_name": { "type": "string" }
                },
                "required": ["task"],
                "additionalProperties": false
            }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ActionId, ActionTaskId};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    struct EchoSpawner;

    #[async_trait]
    impl SubagentSpawner for EchoSpawner {
        async fn spawn_and_run(
            &self,
            task: &str,
            _helper_name: Option<&str>,
        ) -> std::result::Result<String, String> {
            Ok(format!("echo:{task}"))
        }
    }

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[tokio::test]
    async fn spawn_subagent_returns_spawner_result() {
        let skill = SpawnSubagentSkill::new(std::sync::Arc::new(EchoSpawner));
        let req = ActionRequest::new(
            ActionId::new(),
            SpawnSubagentSkill::skill_id(),
            ActionTaskId::new(),
            json!({ "task": "summarize logs" }),
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(
            res.raw_output_redacted["result"],
            json!("echo:summarize logs")
        );
    }

    #[tokio::test]
    async fn disabled_spawner_fails_closed() {
        let skill = SpawnSubagentSkill::disabled();
        let req = ActionRequest::new(
            ActionId::new(),
            SpawnSubagentSkill::skill_id(),
            ActionTaskId::new(),
            json!({ "task": "x" }),
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(!res.status.is_success());
    }
}

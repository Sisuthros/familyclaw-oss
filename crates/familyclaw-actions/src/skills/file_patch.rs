//! Reference pattern: local file patch proposal (Layer A).
//!
//! [`FilePatchMock`] takes a file's content ([`FilePatchInput::file_content`])
//! and a requested change ([`FilePatchInput::requested_edit`]) and produces a
//! **patch proposal** (unified diff). This is a **reference pattern that
//! demonstrates the skill contract** for local disk writes: the execution
//! logic is deterministic and in-memory (it only proposes the change), and
//! the manifest classifies it as a local write ([`crate::policy::ActionRisk::WriteLocal`])
//! which forces human approval before execution. Wire up your own
//! disk-apply component into this execution scaffold when you want to
//! actually write the patch — the approval gate remains in place.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Fixed identifier for the skill, so registration and lookup are reproducible.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444444");

/// Skill input: the original content and the requested change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchInput {
    /// The file's current content.
    pub file_content: String,
    /// The requested change, described in natural language.
    pub requested_edit: String,
}

/// Skill output: the patch proposal (not yet applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchOutput {
    /// The proposed patch as unified diff text.
    pub patch: String,
    /// Whether the patch has been applied to disk (always `false` in the mock).
    pub applied: bool,
}

/// Mock skill for file patch proposals (proposal only).
///
/// The risk class is [`ActionRisk::WriteLocal`] and the policy is
/// [`ApprovalPolicy::AlwaysRequireApproval`], so execution always requires
/// approval (even a local write stops for a human).
#[derive(Debug, Clone, Default)]
pub struct FilePatchMock;

impl FilePatchMock {
    /// Creates a new skill instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The skill's fixed identifier.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Builds the patch proposal (pure logic).
    ///
    /// The mock appends the requested change as a comment line at the end of
    /// the content and produces a simple unified diff. A real diff algorithm
    /// is not needed at the Layer A level — what matters is that the result
    /// is a **proposal**, not an applied change.
    #[must_use]
    pub fn make_patch(input: &FilePatchInput) -> FilePatchOutput {
        let added_line = format!("// edit: {}", input.requested_edit.trim());
        let patch = format!(
            "--- a/file\n+++ b/file\n@@ -1,1 +1,2 @@\n {}\n+{}\n",
            input.file_content.lines().next().unwrap_or(""),
            added_line
        );
        FilePatchOutput {
            patch,
            applied: false,
        }
    }
}

#[async_trait]
impl ActionExecutor for FilePatchMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: FilePatchInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid file_patch input: {e}"),
                    request.now,
                ));
            }
        };

        let out = Self::make_patch(&input);
        let output: Value = json!({
            "patch": out.patch,
            "applied": out.applied,
        });

        Ok(ActionResult::success(
            "produced file patch proposal (not applied)",
            output,
            request.now,
        ))
    }
}

impl Skill for FilePatchMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "file_patch_mock".to_string(),
            version: "1.0.0".to_string(),
            description:
                "Proposes a file change as a patch (does not write to disk without approval)."
                    .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::AlwaysRequireApproval,
            input_hint: Some("{ file_content, requested_edit }".to_string()),
            output_hint: Some("{ patch }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_content": {
                        "type": "string",
                        "description": "The file's current content."
                    },
                    "requested_edit": {
                        "type": "string",
                        "description": "The requested change, described in natural language."
                    }
                },
                "required": ["file_content", "requested_edit"],
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
    use crate::policy::{required_approval, ApprovalRequirement};
    use familyclaw_core::time::{from_unix_secs, Timestamp};

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample() -> FilePatchInput {
        FilePatchInput {
            file_content: "fn main() {}\n".to_string(),
            requested_edit: "add logging".to_string(),
        }
    }

    #[test]
    fn manifest_is_write_local_and_requires_approval() {
        let m = FilePatchMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "file_patch_mock");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        // AlwaysRequireApproval → the policy forces approval even for a
        // local write.
        assert_eq!(
            required_approval(m.risk, m.approval_policy),
            ApprovalRequirement::RequireApproval
        );
    }

    #[test]
    fn make_patch_is_a_proposal_not_applied() {
        let out = FilePatchMock::make_patch(&sample());
        assert!(!out.applied);
        assert!(out.patch.contains("add logging"));
        assert!(out.patch.starts_with("--- a/file"));
    }

    #[tokio::test]
    async fn happy_path_returns_unapplied_patch() {
        let skill = FilePatchMock::new();
        let payload = serde_json::to_value(sample()).expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            FilePatchMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(res.raw_output_redacted["applied"], serde_json::json!(false));
    }
}

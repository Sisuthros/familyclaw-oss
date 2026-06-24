//! Mock-taito: paikallisen tiedoston muutosehdotus (patch) (KERROS A).
//!
//! [`FilePatchMock`] ottaa tiedoston sisällön ([`FilePatchInput::file_content`])
//! ja pyydetyn muutoksen ([`FilePatchInput::requested_edit`]) ja tuottaa
//! **patch-ehdotuksen** (yhtenäinen diff). Taito EI kirjoita levylle — se vain
//! ehdottaa muutoksen. Varsinainen paikallinen kirjoitus
//! ([`crate::policy::ActionRisk::WriteLocal`]) vaatii ihmisen hyväksynnän ennen
//! suoritusta (manifestin käytäntö pakottaa hyväksynnän).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Taidon kiinteä tunniste, jotta rekisteröinti ja haku ovat toistettavia.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("44444444-4444-4444-8444-444444444444");

/// Taidon syöte: alkuperäinen sisältö ja pyydetty muutos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchInput {
    /// Tiedoston nykyinen sisältö.
    pub file_content: String,
    /// Luonnollisella kielellä kuvattu pyydetty muutos.
    pub requested_edit: String,
}

/// Taidon tulos: patch-ehdotus (ei vielä sovellettu).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePatchOutput {
    /// Ehdotettu patch yhtenäisenä diff-tekstinä.
    pub patch: String,
    /// Onko patch sovellettu levylle (mockissa aina `false`).
    pub applied: bool,
}

/// Mock-taito tiedoston patch-ehdotukselle (proposal only).
///
/// Riskiluokka on [`ActionRisk::WriteLocal`] ja käytäntö
/// [`ApprovalPolicy::AlwaysRequireApproval`], joten suoritus vaatii aina
/// hyväksynnän (paikallinenkin kirjoitus pysähtyy ihmiselle).
#[derive(Debug, Clone, Default)]
pub struct FilePatchMock;

impl FilePatchMock {
    /// Luo uuden taidon.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Taidon kiinteä tunniste.
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(SKILL_UUID)
    }

    /// Muodostaa patch-ehdotuksen (puhdas logiikka).
    ///
    /// Mock liittää pyydetyn muutoksen kommenttirivinä sisällön loppuun ja
    /// tuottaa yksinkertaisen yhtenäisen diffin. Oikeaa diff-algoritmia ei
    /// tarvita KERROS A -tasolla — tärkeää on että tulos on **ehdotus**, ei
    /// sovellettu muutos.
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
                "Ehdottaa tiedoston muutoksen patchina (ei kirjoita levylle ilman hyväksyntää)."
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
                        "description": "Tiedoston nykyinen sisältö."
                    },
                    "requested_edit": {
                        "type": "string",
                        "description": "Luonnollisella kielellä kuvattu pyydetty muutos."
                    }
                },
                "required": ["file_content", "requested_edit"],
                "additionalProperties": false
            }),
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
        // AlwaysRequireApproval → policy pakottaa hyväksynnän paikallisellekin
        // kirjoitukselle.
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

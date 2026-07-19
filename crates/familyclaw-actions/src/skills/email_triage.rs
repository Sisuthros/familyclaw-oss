//! Reference pattern: email triage (classification) (Layer A).
//!
//! [`EmailTriageMock`] reads a list of emails ([`EmailItem`]) and classifies
//! each one into a category while suggesting an action. The skill is
//! **read-only** ([`crate::policy::ActionRisk::ReadOnly`]) — it neither sends,
//! deletes, nor modifies anything. This is a **reference pattern that
//! demonstrates the skill contract** (manifest + read-only risk class +
//! input/output schema): the execution logic is deterministic and in-memory,
//! and the sender address is a generic placeholder `user@example.com` (Layer
//! A). Wire up your own Gmail provider into this execution scaffold when you
//! want a live integration — the manifest and pipeline remain unchanged.

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
const SKILL_UUID: uuid::Uuid = uuid::uuid!("22222222-2222-4222-8222-222222222222");

/// A single email to be classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailItem {
    /// The sender's address (generic, e.g. `user@example.com`).
    pub from: String,
    /// The message subject.
    pub subject: String,
    /// The message body.
    pub body: String,
}

/// Skill input: a list of emails to classify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInput {
    /// The emails to classify.
    pub emails: Vec<EmailItem>,
}

/// The classification result for a single email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriagedEmail {
    /// Index in the input list (0-based).
    pub id: usize,
    /// The inferred category (e.g. `urgent`, `spam`, `normal`).
    pub category: String,
    /// The suggested action (e.g. `reply`, `archive`, `read`).
    pub action: String,
}

/// Skill output: the classified emails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageOutput {
    /// Classification results in the order of the input.
    pub categorized: Vec<TriagedEmail>,
}

/// Mock skill for email classification (read-only).
///
/// The risk class is [`ActionRisk::ReadOnly`] and the policy is
/// [`ApprovalPolicy::AutoIfReadOnly`], so execution runs automatically
/// without approval.
#[derive(Debug, Clone, Default)]
pub struct EmailTriageMock;

impl EmailTriageMock {
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

    /// Classifies the emails using deterministic keywords (pure logic).
    ///
    /// Heuristic:
    /// - subject/body contains `urgent`/`asap` → `urgent` + `reply`,
    /// - subject/body contains `unsubscribe`/`offer` → `spam` + `archive`,
    /// - otherwise → `normal` + `read`.
    #[must_use]
    pub fn triage(input: &EmailTriageInput) -> EmailTriageOutput {
        let categorized = input
            .emails
            .iter()
            .enumerate()
            .map(|(id, email)| {
                let haystack = format!("{} {}", email.subject, email.body).to_ascii_lowercase();
                let (category, action) = if haystack.contains("urgent") || haystack.contains("asap")
                {
                    ("urgent", "reply")
                } else if haystack.contains("unsubscribe") || haystack.contains("offer") {
                    ("spam", "archive")
                } else {
                    ("normal", "read")
                };
                TriagedEmail {
                    id,
                    category: category.to_string(),
                    action: action.to_string(),
                }
            })
            .collect();
        EmailTriageOutput { categorized }
    }
}

#[async_trait]
impl ActionExecutor for EmailTriageMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: EmailTriageInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid email_triage input: {e}"),
                    request.now,
                ));
            }
        };

        let out = Self::triage(&input);
        let count = out.categorized.len();
        let output: Value = json!({ "categorized": out.categorized });

        Ok(ActionResult::success(
            format!("triaged {count} email(s)"),
            output,
            request.now,
        ))
    }
}

impl Skill for EmailTriageMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "email_triage_mock".to_string(),
            version: "1.0.0".to_string(),
            description: "Classifies emails into categories and suggests actions (read-only)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ emails: [{ from, subject, body }] }".to_string()),
            output_hint: Some("{ categorized: [{ id, category, action }] }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "emails": {
                        "type": "array",
                        "description": "The emails to classify.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": {
                                    "type": "string",
                                    "description": "The sender's address."
                                },
                                "subject": {
                                    "type": "string",
                                    "description": "The message subject."
                                },
                                "body": {
                                    "type": "string",
                                    "description": "The message body."
                                }
                            },
                            "required": ["from", "subject", "body"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["emails"],
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

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample() -> EmailTriageInput {
        EmailTriageInput {
            emails: vec![
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "URGENT: server down".to_string(),
                    body: "please fix asap".to_string(),
                },
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "Special offer".to_string(),
                    body: "click unsubscribe to opt out".to_string(),
                },
                EmailItem {
                    from: "user@example.com".to_string(),
                    subject: "Lunch?".to_string(),
                    body: "are you free thursday".to_string(),
                },
            ],
        }
    }

    #[test]
    fn manifest_is_read_only_auto() {
        let m = EmailTriageMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "email_triage_mock");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        assert_eq!(m.approval_policy, ApprovalPolicy::AutoIfReadOnly);
    }

    #[test]
    fn triage_classifies_by_keywords() {
        let out = EmailTriageMock::triage(&sample());
        assert_eq!(out.categorized[0].category, "urgent");
        assert_eq!(out.categorized[0].action, "reply");
        assert_eq!(out.categorized[1].category, "spam");
        assert_eq!(out.categorized[2].category, "normal");
    }

    #[tokio::test]
    async fn happy_path_returns_categorized() {
        let skill = EmailTriageMock::new();
        let payload = serde_json::to_value(sample()).expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            EmailTriageMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        let cats = res.raw_output_redacted["categorized"]
            .as_array()
            .expect("array");
        assert_eq!(cats.len(), 3);
    }
}

//! Reference pattern: Discord thread summarization (Layer A).
//!
//! [`DiscordThreadSummaryMock`] reads the messages of a thread ([`ThreadMessage`])
//! and produces a short summary along with extracted action item suggestions.
//! The skill is **read-only** ([`crate::policy::ActionRisk::ReadOnly`]) — it
//! neither sends nor modifies anything. This is a **reference pattern that
//! demonstrates the skill contract**: the execution logic is deterministic and
//! in-memory, and the names used are generic placeholders (`agent_a`,
//! `agent_b`, channel `general`). Wire up your own Discord API provider into
//! this execution scaffold when you want to read a real thread — the manifest
//! and pipeline remain unchanged.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Generic channel (Layer A — not a real channel).
pub const CHANNEL: &str = "general";

/// Fixed identifier for the skill, so registration and lookup are reproducible.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("33333333-3333-4333-8333-333333333333");

/// A single message in a thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    /// The message author (generic, e.g. `agent_a`).
    pub author: String,
    /// The text content of the message.
    pub text: String,
}

/// Skill input: the thread's messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordThreadSummaryInput {
    /// The messages of the thread to summarize, in order.
    pub thread: Vec<ThreadMessage>,
}

/// Skill output: summary + action item suggestions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordThreadSummaryOutput {
    /// A short summary of the thread.
    pub summary: String,
    /// Extracted action item suggestions.
    pub action_items: Vec<String>,
}

/// Mock skill for Discord thread summarization (read-only).
///
/// The risk class is [`ActionRisk::ReadOnly`] and the policy is
/// [`ApprovalPolicy::AutoIfReadOnly`], so execution runs automatically.
#[derive(Debug, Clone, Default)]
pub struct DiscordThreadSummaryMock;

impl DiscordThreadSummaryMock {
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

    /// Summarizes the thread deterministically (pure logic).
    ///
    /// The summary states the participant count and message count; action
    /// item suggestions are extracted from messages that contain the word
    /// `todo`/`action`/`pitää`/`should`.
    #[must_use]
    pub fn summarize(input: &DiscordThreadSummaryInput) -> DiscordThreadSummaryOutput {
        let msg_count = input.thread.len();
        let mut authors: Vec<&str> = input.thread.iter().map(|m| m.author.as_str()).collect();
        authors.sort_unstable();
        authors.dedup();

        let summary = format!(
            "{} message(s) from {} participant(s) in channel {CHANNEL}.",
            msg_count,
            authors.len()
        );

        let action_items = input
            .thread
            .iter()
            .filter(|m| {
                let lower = m.text.to_ascii_lowercase();
                lower.contains("todo")
                    || lower.contains("action")
                    || lower.contains("pitää")
                    || lower.contains("should")
            })
            .map(|m| format!("{}: {}", m.author, m.text.trim()))
            .collect();

        DiscordThreadSummaryOutput {
            summary,
            action_items,
        }
    }
}

#[async_trait]
impl ActionExecutor for DiscordThreadSummaryMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: DiscordThreadSummaryInput = match serde_json::from_value(request.payload.clone())
        {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid discord_thread_summary input: {e}"),
                    request.now,
                ));
            }
        };

        let out = Self::summarize(&input);
        let output: Value = json!({
            "summary": out.summary,
            "action_items": out.action_items,
        });

        Ok(ActionResult::success(
            format!("summarized thread in channel {CHANNEL}"),
            output,
            request.now,
        ))
    }
}

impl Skill for DiscordThreadSummaryMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "discord_thread_summary_mock".to_string(),
            version: "1.0.0".to_string(),
            description: "Summarizes a Discord thread and extracts action items (read-only)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ thread: [{ author, text }] }".to_string()),
            output_hint: Some("{ summary, action_items }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "thread": {
                        "type": "array",
                        "description": "The messages of the thread to summarize, in order.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "author": {
                                    "type": "string",
                                    "description": "The message author."
                                },
                                "text": {
                                    "type": "string",
                                    "description": "The text content of the message."
                                }
                            },
                            "required": ["author", "text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["thread"],
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

    fn sample() -> DiscordThreadSummaryInput {
        DiscordThreadSummaryInput {
            thread: vec![
                ThreadMessage {
                    author: "agent_a".to_string(),
                    text: "We should ship the fix today".to_string(),
                },
                ThreadMessage {
                    author: "agent_b".to_string(),
                    text: "Agreed, looks good".to_string(),
                },
                ThreadMessage {
                    author: "agent_a".to_string(),
                    text: "TODO: write the changelog".to_string(),
                },
            ],
        }
    }

    #[test]
    fn manifest_is_read_only_auto() {
        let m = DiscordThreadSummaryMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "discord_thread_summary_mock");
        assert_eq!(m.risk, ActionRisk::ReadOnly);
        // The input schema is a proper JSON object with a `thread` array.
        assert_eq!(m.input_schema["type"], "object");
        assert!(m.input_schema["properties"]["thread"].is_object());
    }

    #[test]
    fn summarize_counts_and_extracts_actions() {
        let out = DiscordThreadSummaryMock::summarize(&sample());
        assert!(out.summary.contains("3 message"));
        assert!(out.summary.contains("2 participant"));
        // "should" and "TODO" lines become action items.
        assert_eq!(out.action_items.len(), 2);
    }

    #[tokio::test]
    async fn happy_path_returns_summary() {
        let skill = DiscordThreadSummaryMock::new();
        let payload = serde_json::to_value(sample()).expect("serialize");
        let req = ActionRequest::new(
            ActionId::new(),
            DiscordThreadSummaryMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert!(res.raw_output_redacted["summary"]
            .as_str()
            .expect("summary string")
            .contains(CHANNEL));
    }
}

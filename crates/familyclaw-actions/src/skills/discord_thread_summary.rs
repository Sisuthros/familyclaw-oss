//! Mock-taito: Discord-keskusteluketjun tiivistys (KERROS A).
//!
//! [`DiscordThreadSummaryMock`] lukee keskusteluketjun viestit ([`ThreadMessage`])
//! ja tuottaa lyhyen tiivistelmän sekä poimitut toimenpide-ehdotukset. Taito on
//! **vain luku** ([`crate::policy::ActionRisk::ReadOnly`]) — se ei lähetä eikä
//! muokkaa mitään, eikä tee oikeita Discord-verkkokutsuja. Käytetyt nimet ovat
//! geneerisiä paikkamerkkejä (`agent_a`, `agent_b`, kanava `general`).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::MockSkill;

/// Geneerinen kanava (KERROS A — ei oikea kanava).
pub const CHANNEL: &str = "general";

/// Taidon kiinteä tunniste, jotta rekisteröinti ja haku ovat toistettavia.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("33333333-3333-4333-8333-333333333333");

/// Yksittäinen viesti keskusteluketjussa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMessage {
    /// Viestin kirjoittaja (geneerinen, esim. `agent_a`).
    pub author: String,
    /// Viestin tekstisisältö.
    pub text: String,
}

/// Taidon syöte: keskusteluketjun viestit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordThreadSummaryInput {
    /// Tiivistettävän ketjun viestit järjestyksessä.
    pub thread: Vec<ThreadMessage>,
}

/// Taidon tulos: tiivistelmä + toimenpide-ehdotukset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordThreadSummaryOutput {
    /// Lyhyt tiivistelmä ketjusta.
    pub summary: String,
    /// Poimitut toimenpide-ehdotukset.
    pub action_items: Vec<String>,
}

/// Mock-taito Discord-ketjun tiivistykselle (vain luku).
///
/// Riskiluokka on [`ActionRisk::ReadOnly`] ja käytäntö
/// [`ApprovalPolicy::AutoIfReadOnly`], joten suoritus ajaa automaattisesti.
#[derive(Debug, Clone, Default)]
pub struct DiscordThreadSummaryMock;

impl DiscordThreadSummaryMock {
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

    /// Tiivistää ketjun deterministisesti (puhdas logiikka).
    ///
    /// Tiivistelmä kertoo osallistujamäärän ja viestien lukumäärän; toimenpide-
    /// ehdotuksiksi poimitaan viestit, jotka sisältävät sanan `todo`/`action`/
    /// `pitää`/`should`.
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

impl MockSkill for DiscordThreadSummaryMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "discord_thread_summary_mock".to_string(),
            version: "1.0.0".to_string(),
            description: "Tiivistää Discord-keskusteluketjun ja poimii toimenpiteet (vain luku)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: Some("{ thread: [{ author, text }] }".to_string()),
            output_hint: Some("{ summary, action_items }".to_string()),
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

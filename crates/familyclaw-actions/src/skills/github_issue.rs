//! GitHub-issue-taito: luonnos tai oikea issue `GITHUB_TOKEN`:lla (KERROS A).
//!
//! Kun `create` on `false`, taito tuottaa vain luonnoksen (ei verkkokutsua).
//! Kun `create` on `true`, POST GitHub REST API:in — [`ActionRisk::WriteExternal`]
//! + hyväksyntä ennen suoritusta.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Geneerinen esimerkkirepo (KERROS A).
pub const EXAMPLE_REPO: &str = "example-org/example-repo";

/// Sama UUID kuin aiemmalla mock-taidolla — taaksepäin-yhteensopiva rekisteröinti.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("11111111-1111-4111-8111-111111111111");

/// Syöte GitHub-issue-taidolle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueInput {
    /// Kohderepo `owner/repo` muodossa.
    #[serde(default = "default_repo")]
    pub repo: String,
    /// Issuen otsikko.
    #[serde(default)]
    pub title: String,
    /// Issuen runko (Markdown).
    #[serde(default)]
    pub body: String,
    /// Vapaamuotoinen bugiraportti (vaihtoehto title/body:lle).
    #[serde(default)]
    pub bug_report: Option<String>,
    /// `true` → luo oikea issue API:lla (vaatii hyväksynnän). `false` → vain luonnos.
    #[serde(default)]
    pub create: bool,
}

fn default_repo() -> String {
    EXAMPLE_REPO.to_string()
}

/// Tulos GitHub-issue-taidolle (luonnos tai luotu issue).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueOutput {
    /// Issuen otsikko.
    pub title: String,
    /// Issuen runko (Markdown).
    pub body: String,
    /// Kohderepo `owner/repo` muodossa.
    pub repo: String,
    /// `true` jos issue luotiin oikeasti API:lla (`false` = luonnos).
    pub created: bool,
    /// Luodun issuen numero (vain kun `created`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,
    /// Luodun issuen URL (vain kun `created`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
}

/// Aito GitHub-issue-taito (`GITHUB_TOKEN` + reqwest).
#[derive(Debug, Clone, Default)]
pub struct GithubIssueSkill;

impl GithubIssueSkill {
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

    /// Muodostaa issue-luonnoksen syötteestä (ei kutsu API:a).
    #[must_use]
    pub fn build_draft(input: &GithubIssueInput) -> GithubIssueOutput {
        let (title, body) = if let Some(report) = input.bug_report.as_ref() {
            let first_line = report
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("Bug report");
            let title: String = first_line.chars().take(72).collect();
            let body = format!(
                "## Yhteenveto\n{}\n\n## Toistaminen\n1. (täydennä)\n\n_Luonnos repoon {}._",
                report.trim(),
                input.repo
            );
            (title, body)
        } else {
            (input.title.clone(), input.body.clone())
        };
        GithubIssueOutput {
            title,
            body,
            repo: input.repo.clone(),
            created: false,
            issue_number: None,
            issue_url: None,
        }
    }

    async fn create_issue(
        token: &str,
        repo: &str,
        title: &str,
        body: &str,
    ) -> std::result::Result<(u64, String), String> {
        let url = format!("https://api.github.com/repos/{repo}/issues");
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "familyclaw-agent")
            .json(&json!({ "title": title, "body": body }))
            .send()
            .await
            .map_err(|e| format!("github request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            return Err(format!("github API {status}: {detail}"));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| format!("github response parse: {e}"))?;
        let number = v["number"]
            .as_u64()
            .ok_or_else(|| "missing issue number".to_string())?;
        let html_url = v["html_url"].as_str().unwrap_or("").to_string();
        Ok((number, html_url))
    }
}

#[async_trait]
impl ActionExecutor for GithubIssueSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: GithubIssueInput = match serde_json::from_value(request.payload.clone()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid github_issue input: {e}"),
                    request.now,
                ));
            }
        };

        let mut draft = Self::build_draft(&input);

        if input.create {
            let token = match std::env::var("GITHUB_TOKEN") {
                Ok(t) if !t.trim().is_empty() => t,
                _ => {
                    return Ok(ActionResult::failure(
                        "GITHUB_TOKEN required for create=true",
                        request.now,
                    ));
                }
            };
            match Self::create_issue(token.trim(), &draft.repo, &draft.title, &draft.body).await {
                Ok((num, url)) => {
                    draft.created = true;
                    draft.issue_number = Some(num);
                    draft.issue_url = Some(url);
                }
                Err(e) => {
                    return Ok(ActionResult::failure(e, request.now));
                }
            }
        }

        let output: Value = json!({
            "draft": {
                "title": draft.title,
                "body": draft.body,
                "repo": draft.repo,
            },
            "created": draft.created,
            "issue_number": draft.issue_number,
            "issue_url": draft.issue_url,
        });

        let summary = if draft.created {
            format!(
                "created github issue #{} on {}",
                draft.issue_number.unwrap_or(0),
                draft.repo
            )
        } else {
            format!("drafted github issue '{}' for {}", draft.title, draft.repo)
        };

        Ok(ActionResult::success(summary, output, request.now))
    }
}

impl Skill for GithubIssueSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "github_issue".to_string(),
            version: "2.0.0".to_string(),
            description:
                "Luonnostelee tai luo GitHub-issuen (GITHUB_TOKEN, create=true vaatii hyväksynnän)."
                    .to_string(),
            permissions: vec![SkillPermission::WriteExternal],
            risk: ActionRisk::WriteExternal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{ repo?, title, body?, bug_report?, create? }".to_string()),
            output_hint: Some("{ draft, created, issue_number?, issue_url? }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "bug_report": { "type": "string" },
                    "create": { "type": "boolean" }
                },
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

    #[test]
    fn draft_from_bug_report() {
        let input = GithubIssueInput {
            repo: EXAMPLE_REPO.to_string(),
            title: String::new(),
            body: String::new(),
            bug_report: Some("Crash on startup".to_string()),
            create: false,
        };
        let out = GithubIssueSkill::build_draft(&input);
        assert_eq!(out.title, "Crash on startup");
        assert!(!out.created);
    }

    #[tokio::test]
    async fn draft_only_without_create_flag() {
        let skill = GithubIssueSkill::new();
        let req = ActionRequest::new(
            ActionId::new(),
            GithubIssueSkill::skill_id(),
            ActionTaskId::new(),
            json!({
                "title": "Test issue",
                "body": "Details",
                "create": false
            }),
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(res.raw_output_redacted["created"], json!(false));
    }
}

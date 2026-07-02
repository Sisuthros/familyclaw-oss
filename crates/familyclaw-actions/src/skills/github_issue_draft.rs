//! Esimerkkimalli (reference pattern): GitHub-issuen luonnos bugiraportista (KERROS A).
//!
//! [`GithubIssueDraftMock`] muuntaa vapaamuotoisen bugiraportin
//! ([`GithubIssueDraftInput::bug_report`]) jäsennellyksi issue-**luonnokseksi**
//! (otsikko + runko). Tämä on **referenssimalli joka näyttää taidon sopimuksen**
//! erityisesti hyväksyntää vaativalle kirjoitustaidolle: suorituslogiikka on
//! deterministinen ja muistinvarainen (se vain ehdottaa sisällön), mutta
//! manifesti luokittelee sen ulkoiseksi kirjoitukseksi
//! ([`crate::policy::ActionRisk::WriteExternal`]) joka vaatii ihmisen
//! hyväksynnän ennen suoritusta. Repo-slug on geneerinen placeholder
//! `example-org/example-repo` (KERROS A — ei oikeita kohteita). Kytke oma
//! GitHub-API-tarjoajasi tähän suoritusrunkoon, kun haluat luoda oikean issuen —
//! hyväksyntäportti ja todiste-redaktio pätevät silloin oikeaan sivuvaikutukseen.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::Result;
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Geneerinen kohderepo (KERROS A — ei oikea repo).
pub const EXAMPLE_REPO: &str = "example-org/example-repo";

/// Taidon kiinteä tunniste, jotta rekisteröinti ja haku ovat toistettavia.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("11111111-1111-4111-8111-111111111111");

/// Taidon syöte: vapaamuotoinen bugiraportti.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueDraftInput {
    /// Käyttäjän kirjoittama vapaamuotoinen bugiraportti.
    pub bug_report: String,
}

/// Taidon tulos: issue-luonnos (ei vielä luotu mihinkään).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueDraftOutput {
    /// Ehdotettu issuen otsikko.
    pub title: String,
    /// Ehdotettu issuen runko (Markdown).
    pub body: String,
    /// Kohderepo johon issue ehdotetaan (geneerinen).
    pub repo: String,
}

/// Mock-taito GitHub-issuen luonnokselle (proposal only).
///
/// Riskiluokka on [`ActionRisk::WriteExternal`] ja käytäntö
/// [`ApprovalPolicy::RequireApproval`], joten suoritus vaatii aina hyväksynnän.
#[derive(Debug, Clone, Default)]
pub struct GithubIssueDraftMock;

impl GithubIssueDraftMock {
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

    /// Muodostaa issue-luonnoksen bugiraportista (puhdas logiikka).
    ///
    /// Otsikko johdetaan raportin ensimmäisestä rivistä (typistettynä), ja runko
    /// sisältää alkuperäisen raportin sekä geneerisen toistorakenteen.
    #[must_use]
    pub fn draft(input: &GithubIssueDraftInput) -> GithubIssueDraftOutput {
        let first_line = input
            .bug_report
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("Bug report");
        let title: String = first_line.chars().take(72).collect();

        let body = format!(
            "## Yhteenveto\n{}\n\n## Toistaminen\n1. (täydennä vaiheet)\n\n## Odotettu vs. todellinen\n- Odotettu: (täydennä)\n- Todellinen: (täydennä)\n\n_Luonnos — odottaa hyväksyntää ennen julkaisua repoon {EXAMPLE_REPO}._",
            input.bug_report.trim()
        );

        GithubIssueDraftOutput {
            title,
            body,
            repo: EXAMPLE_REPO.to_string(),
        }
    }
}

#[async_trait]
impl ActionExecutor for GithubIssueDraftMock {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        // Jäsennä syöte; epäkelpo syöte tuottaa Failed-tuloksen (ei virhettä).
        let input: GithubIssueDraftInput = match serde_json::from_value(request.payload.clone()) {
            Ok(input) => input,
            Err(e) => {
                return Ok(ActionResult::failure(
                    format!("invalid github_issue_draft input: {e}"),
                    request.now,
                ));
            }
        };

        let draft = Self::draft(&input);
        let output: Value = json!({
            "draft": {
                "title": draft.title,
                "body": draft.body,
                "repo": draft.repo,
            },
            "published": false
        });

        Ok(ActionResult::success(
            format!("drafted github issue '{}' for {EXAMPLE_REPO}", draft.title),
            output,
            request.now,
        ))
    }
}

impl Skill for GithubIssueDraftMock {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "github_issue_draft".to_string(),
            version: "1.0.0".to_string(),
            description: "Luonnostelee GitHub-issuen bugiraportista (vain ehdotus, ei julkaisua)."
                .to_string(),
            permissions: vec![SkillPermission::WriteExternal],
            risk: ActionRisk::WriteExternal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{ bug_report: string }".to_string()),
            output_hint: Some("{ title, body, repo }".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "bug_report": {
                        "type": "string",
                        "description": "Käyttäjän kirjoittama vapaamuotoinen bugiraportti."
                    }
                },
                "required": ["bug_report"],
                "additionalProperties": false
            }),
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
    fn manifest_is_write_external_and_requires_approval() {
        let m = GithubIssueDraftMock::new().manifest();
        m.validate().expect("manifest validates");
        assert_eq!(m.name, "github_issue_draft");
        assert_eq!(m.risk, ActionRisk::WriteExternal);
        assert!(m.permissions.contains(&SkillPermission::WriteExternal));
        assert!(m.approval_policy.can_require_approval());
        // Syöteskeema vaatii `bug_report`-merkkijonon.
        assert_eq!(m.input_schema["type"], "object");
        assert_eq!(m.input_schema["required"][0], "bug_report");
    }

    #[test]
    fn draft_builds_title_and_body() {
        let input = GithubIssueDraftInput {
            bug_report: "Crash on startup\nIt explodes when config is empty".to_string(),
        };
        let out = GithubIssueDraftMock::draft(&input);
        assert_eq!(out.title, "Crash on startup");
        assert!(out.body.contains("It explodes"));
        assert_eq!(out.repo, EXAMPLE_REPO);
    }

    #[tokio::test]
    async fn happy_path_produces_unpublished_draft() {
        let skill = GithubIssueDraftMock::new();
        let payload = serde_json::to_value(GithubIssueDraftInput {
            bug_report: "Login button does nothing".to_string(),
        })
        .expect("serialize input");
        let req = ActionRequest::new(
            ActionId::new(),
            GithubIssueDraftMock::skill_id(),
            ActionTaskId::new(),
            payload,
            at(1_700_000_000),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(
            res.raw_output_redacted["published"],
            serde_json::json!(false)
        );
        assert_eq!(
            res.raw_output_redacted["draft"]["repo"],
            serde_json::json!(EXAMPLE_REPO)
        );
    }

    #[tokio::test]
    async fn invalid_input_fails_gracefully() {
        let skill = GithubIssueDraftMock::new();
        let req = ActionRequest::new(
            ActionId::new(),
            GithubIssueDraftMock::skill_id(),
            ActionTaskId::new(),
            serde_json::json!({ "wrong": "shape" }),
            at(1),
        );
        let res = skill.execute(req).await.expect("execute");
        assert!(!res.status.is_success());
    }
}

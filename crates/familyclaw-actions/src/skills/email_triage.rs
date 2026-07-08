//! Esimerkkimalli (reference pattern): sähköpostien luokittelu (triage) (KERROS A).
//!
//! [`EmailTriageMock`] lukee listan sähköposteja ([`EmailItem`]) ja luokittelee
//! kunkin kategoriaan sekä ehdottaa toimenpidettä. Taito on **vain luku**
//! ([`crate::policy::ActionRisk::ReadOnly`]) — se ei lähetä, poista eikä muokkaa
//! mitään. Tämä on **referenssimalli joka näyttää taidon sopimuksen**
//! (manifesti + read-only-riskiluokka + syöte/tuloste-skeema): suorituslogiikka
//! on deterministinen ja muistinvarainen, ja lähettäjäosoite on geneerinen
//! placeholder `user@example.com` (KERROS A). Kytke oma Gmail-tarjoajasi tähän
//! suoritusrunkoon, kun haluat elävän integraation — manifesti ja putki pysyvät
//! ennallaan.

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
const SKILL_UUID: uuid::Uuid = uuid::uuid!("22222222-2222-4222-8222-222222222222");

/// Yksittäinen sähköposti luokiteltavaksi.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailItem {
    /// Lähettäjän osoite (geneerinen, esim. `user@example.com`).
    pub from: String,
    /// Viestin aihe.
    pub subject: String,
    /// Viestin runko.
    pub body: String,
}

/// Taidon syöte: lista luokiteltavia sähköposteja.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageInput {
    /// Luokiteltavat sähköpostit.
    pub emails: Vec<EmailItem>,
}

/// Yhden sähköpostin luokittelutulos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriagedEmail {
    /// Indeksi syötelistassa (0-pohjainen).
    pub id: usize,
    /// Päätelty kategoria (esim. `urgent`, `spam`, `normal`).
    pub category: String,
    /// Ehdotettu toimenpide (esim. `reply`, `archive`, `read`).
    pub action: String,
}

/// Taidon tulos: luokitellut sähköpostit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTriageOutput {
    /// Luokittelutulokset syötteen järjestyksessä.
    pub categorized: Vec<TriagedEmail>,
}

/// Mock-taito sähköpostien luokittelulle (vain luku).
///
/// Riskiluokka on [`ActionRisk::ReadOnly`] ja käytäntö
/// [`ApprovalPolicy::AutoIfReadOnly`], joten suoritus ajaa automaattisesti
/// ilman hyväksyntää.
#[derive(Debug, Clone, Default)]
pub struct EmailTriageMock;

impl EmailTriageMock {
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

    /// Luokittelee sähköpostit deterministisillä avainsanoilla (puhdas logiikka).
    ///
    /// Heuristiikka:
    /// - aihe/runko sisältää sanan `urgent`/`asap` → `urgent` + `reply`,
    /// - aihe/runko sisältää sanan `unsubscribe`/`offer` → `spam` + `archive`,
    /// - muutoin → `normal` + `read`.
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
            description:
                "Luokittelee sähköpostit kategorioihin ja ehdottaa toimenpiteet (vain luku)."
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
                        "description": "Luokiteltavat sähköpostit.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": {
                                    "type": "string",
                                    "description": "Lähettäjän osoite."
                                },
                                "subject": {
                                    "type": "string",
                                    "description": "Viestin aihe."
                                },
                                "body": {
                                    "type": "string",
                                    "description": "Viestin runko."
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

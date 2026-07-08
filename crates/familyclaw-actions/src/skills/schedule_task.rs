//! Ajastettu tehtävä agency-configiin (KERROS A).
//!
//! [`ScheduleTaskSkill`] mahdollistaa agentin rekisteröidä cron-pohjaisen
//! ajastetun tehtävän persistoituun [`AgencyConfig`]-tiedostoon (`agency.json`).
//! Taito kirjoittaa geneerisen merkinnän (cron-lauseke + `skill_id` + payload)
//! — ei kovakoodattuja polkuja eikä henkilökohtaisia tunnisteita.
//!
//! ## Turvallisuus
//! Riskiluokka on [`ActionRisk::WriteLocal`] ja käytäntö vaatii ihmisen
//! hyväksynnän ennen kirjoitusta. Config-polku annetaan payloadissa
//! (`agency_config_path`) — operaattori hallitsee sallitut polut (KERROS B).

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult};
use crate::ids::SkillId;
use crate::manifest::SkillManifest;
use crate::policy::{ActionRisk, ApprovalPolicy, SkillPermission};

use super::Skill;

/// Taidon kiinteä tunniste.
const SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaf");

/// Geneerinen oletus-olento ajastetuille tehtäthe operator.
const DEFAULT_BEING_ID: &str = "operator";

/// Taidon syöte: agency-config-polku + ajastettavan tehtävän määrittely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleTaskInput {
    /// Polku `agency.json`-tiedostoon (operaattorin datahakemisto).
    pub agency_config_path: String,
    /// Valinnainen vakaa tehtävä-id (UUID-merkkijono). Jos puuttuu, luodaan uusi.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Suoritettavan taidon tunniste (UUID-merkkijono).
    pub skill_id: String,
    /// Taidolle annettava geneerinen JSON-payload.
    pub payload: Value,
    /// Cron-lauseke (esim. `"0 */6 * * *"`).
    pub cron_expression: String,
    /// Valinnainen olentotunniste lähetykselle (oletus `operator`).
    #[serde(default)]
    pub being_id: Option<String>,
}

/// Yhden persistoidun ajastetun tehtävän merkintä agency-configissa.
///
/// Peilaa [`familyclaw_scheduler::persistence::AgencyScheduledTask`]:a —
/// pidetään synkassa manuaalisesti (ei riippuvuutta scheduler-crateen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgencyScheduledTaskEntry {
    id: String,
    skill_id: String,
    payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cron_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    interval_secs: Option<i64>,
    #[serde(default = "default_being_id")]
    being_id: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_being_id() -> String {
    DEFAULT_BEING_ID.to_string()
}

const fn default_enabled() -> bool {
    true
}

/// Agency-configin JSON-muoto (yhteensopiva scheduler-craten kanssa).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AgencyConfigFile {
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    scheduled_tasks: Vec<AgencyScheduledTaskEntry>,
}

impl AgencyConfigFile {
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| ActionError::Proof(format!("agency config parse failed: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ActionError::Proof(format!(
                "agency config read failed: {e}"
            ))),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ActionError::Proof(format!("agency config dir failed: {e}")))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| ActionError::Proof(format!("agency config serialize failed: {e}")))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| ActionError::Proof(format!("agency config write failed: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| ActionError::Proof(format!("agency config rename failed: {e}")))?;
        Ok(())
    }

    fn upsert(&mut self, entry: AgencyScheduledTaskEntry) {
        if let Some(slot) = self
            .scheduled_tasks
            .iter_mut()
            .find(|task| task.id == entry.id)
        {
            *slot = entry;
        } else {
            self.scheduled_tasks.push(entry);
        }
    }
}

/// Taito ajastetun tehtävän rekisteröintiin agency-configiin.
#[derive(Debug, Default, Clone)]
pub struct ScheduleTaskSkill;

impl ScheduleTaskSkill {
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

    /// Validoi syötteen ja muodostaa persistoitavan merkinnän.
    // Palauttaa crate-yksityisen `AgencyScheduledTaskEntry`-tyypin, joten
    // näkyvyys on `pub(crate)` (ei julkista API-pintaa).
    pub(crate) fn build_entry(input: &ScheduleTaskInput) -> Result<AgencyScheduledTaskEntry> {
        let cron = input.cron_expression.trim();
        if cron.is_empty() {
            return Err(ActionError::Proof(
                "cron_expression must not be empty".to_string(),
            ));
        }
        Uuid::parse_str(&input.skill_id).map_err(|_| {
            ActionError::Proof(format!("invalid skill_id UUID: {}", input.skill_id))
        })?;
        let id = match &input.task_id {
            Some(raw) => {
                Uuid::parse_str(raw)
                    .map_err(|_| ActionError::Proof(format!("invalid task_id UUID: {raw}")))?;
                raw.clone()
            }
            None => Uuid::new_v4().to_string(),
        };
        Ok(AgencyScheduledTaskEntry {
            id,
            skill_id: input.skill_id.clone(),
            payload: input.payload.clone(),
            cron_expression: Some(cron.to_string()),
            interval_secs: None,
            being_id: input
                .being_id
                .clone()
                .unwrap_or_else(|| DEFAULT_BEING_ID.to_string()),
            enabled: true,
        })
    }

    /// Kirjoittaa tai päivittää ajastetun tehtävän agency-config-tiedostoon.
    // Palauttaa crate-yksityisen `AgencyScheduledTaskEntry`-tyypin, joten
    // näkyvyys on `pub(crate)` (ei julkista API-pintaa).
    pub(crate) fn register(input: &ScheduleTaskInput) -> Result<AgencyScheduledTaskEntry> {
        let path = Path::new(&input.agency_config_path);
        let entry = Self::build_entry(input)?;
        let mut cfg = AgencyConfigFile::load(path)?;
        cfg.upsert(entry.clone());
        cfg.save(path)?;
        Ok(entry)
    }
}

#[async_trait]
impl ActionExecutor for ScheduleTaskSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let input: ScheduleTaskInput = serde_json::from_value(request.payload.clone())
            .map_err(|e| ActionError::Proof(format!("invalid schedule_task input: {e}")))?;
        let entry = Self::register(&input)?;
        Ok(ActionResult::success(
            "scheduled task registered in agency config",
            json!({
                "task_id": entry.id,
                "skill_id": entry.skill_id,
                "cron_expression": entry.cron_expression,
                "being_id": entry.being_id,
            }),
            request.now,
        ))
    }
}

impl Skill for ScheduleTaskSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "schedule_task".to_string(),
            version: "1.0.0".to_string(),
            description: "Register a cron-scheduled task in agency config (generic payload)."
                .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("agency_config_path, skill_id, payload, cron_expression".to_string()),
            output_hint: Some("task_id and cron_expression".to_string()),
            input_schema: json!({
                "type": "object",
                "required": ["agency_config_path", "skill_id", "payload", "cron_expression"],
                "properties": {
                    "agency_config_path": { "type": "string" },
                    "task_id": { "type": "string" },
                    "skill_id": { "type": "string" },
                    "payload": { "type": "object" },
                    "cron_expression": { "type": "string" },
                    "being_id": { "type": "string" }
                }
            }),
            publisher: None,
            signature: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;

    fn at(secs: i64) -> familyclaw_core::time::Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[test]
    fn build_entry_requires_valid_uuids_and_cron() {
        let input = ScheduleTaskInput {
            agency_config_path: "/tmp/agency.json".to_string(),
            task_id: None,
            skill_id: SkillId::new().to_string(),
            payload: json!({ "k": 1 }),
            cron_expression: "0 * * * *".to_string(),
            being_id: None,
        };
        let entry = ScheduleTaskSkill::build_entry(&input).expect("valid entry");
        assert_eq!(entry.cron_expression.as_deref(), Some("0 * * * *"));
        assert!(entry.interval_secs.is_none());

        let bad_cron = ScheduleTaskInput {
            cron_expression: "   ".to_string(),
            ..input.clone()
        };
        assert!(ScheduleTaskSkill::build_entry(&bad_cron).is_err());
    }

    #[tokio::test]
    async fn register_persists_to_agency_config_file() {
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-schedule-task-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("agency.json");
        let skill_id = SkillId::new().to_string();
        let input = ScheduleTaskInput {
            agency_config_path: path.to_string_lossy().to_string(),
            task_id: Some(Uuid::from_u128(42).to_string()),
            skill_id: skill_id.clone(),
            payload: json!({ "action": "ping" }),
            cron_expression: "0 */6 * * *".to_string(),
            being_id: Some("agent_a".to_string()),
        };
        let skill = ScheduleTaskSkill::new();
        let result = skill
            .execute(ActionRequest::new(
                crate::ids::ActionId::new(),
                ScheduleTaskSkill::skill_id(),
                crate::ids::ActionTaskId::new(),
                serde_json::to_value(&input).expect("serialize input"),
                at(1_700_000_000),
            ))
            .await
            .expect("execute");
        assert!(result.status.is_success());

        let loaded = AgencyConfigFile::load(&path).expect("load saved config");
        assert_eq!(loaded.scheduled_tasks.len(), 1);
        assert_eq!(loaded.scheduled_tasks[0].skill_id, skill_id);
        assert_eq!(
            loaded.scheduled_tasks[0].cron_expression.as_deref(),
            Some("0 */6 * * *")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

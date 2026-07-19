//! **Config persistence** for family-agency state (Phase 4, D5).
//!
//! The kill-switch state of scheduled tasks ([`crate::ScheduledTask::enabled`])
//! must survive a process restart: if an operator stopped a task, it must
//! remain stopped after a restart as well. This module stores that state in a
//! **separate config file** (e.g. `<data_dir>/agency.json`), NOT in the
//! durable-replay journal — the D5 roadmap specifically warns against mixing
//! timer/agency state into the replay substrate (different lifecycle,
//! different ownership).
//!
//! The format is deliberately minimal: just a list of **disabled task ids**
//! as UUID strings. Enabled tasks are the default, so they don't need to be
//! listed → the file stays small and diff-friendly.
//!
//! ```json
//! { "disabled": ["d4ea3c1c-0000-4000-8000-647265616d63"] }
//! ```
//!
//! I/O is **synchronous** (`std::fs`): state is read once at boot and written
//! only on rare operator mutations, so the async overhead isn't justified. A
//! missing file means an empty config (not an error) — first-time startup.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use chrono::Duration;
use familyclaw_actions::SkillId;

use crate::dispatch::Scheduler;
use crate::task::{ScheduledTask, ScheduledTaskId};

/// A scheduled task persisted in the agency config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgencyScheduledTask {
    /// The task's stable identifier (UUID string).
    pub id: String,
    /// The identifier of the skill to execute (UUID string).
    pub skill_id: String,
    /// The generic JSON payload passed to the skill.
    pub payload: Value,
    /// Cron expression; when set, overrides `interval_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expression: Option<String>,
    /// Interval in seconds (backward-compatible; used if no cron is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<i64>,
    /// The generic identifier of the being to dispatch on behalf of.
    #[serde(default = "default_being_id")]
    pub being_id: String,
    /// Whether the task is active (defaults to `true`).
    #[serde(default = "default_task_enabled")]
    pub enabled: bool,
}

fn default_being_id() -> String {
    "operator".to_string()
}

const fn default_task_enabled() -> bool {
    true
}

impl AgencyScheduledTask {
    /// Converts this config entry into a scheduler [`ScheduledTask`].
    ///
    /// Returns an error if the identifiers aren't valid UUIDs or if no
    /// schedule can be derived (neither cron nor interval set).
    pub fn to_scheduled_task(&self) -> Result<ScheduledTask, String> {
        let id =
            Uuid::parse_str(&self.id).map_err(|e| format!("invalid task id {}: {e}", self.id))?;
        let skill = Uuid::parse_str(&self.skill_id)
            .map_err(|e| format!("invalid skill id {}: {e}", self.skill_id))?;
        let interval = self
            .interval_secs
            .map_or_else(|| Duration::seconds(60), Duration::seconds);
        let mut task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(id),
            SkillId::from_uuid(skill),
            self.payload.clone(),
            interval,
            self.being_id.clone(),
        )
        .with_enabled(self.enabled);
        if let Some(ref cron) = self.cron_expression {
            task = task.with_cron_expression(cron);
        } else if self.interval_secs.is_none() {
            return Err("scheduled task needs cron_expression or interval_secs".to_string());
        }
        Ok(task)
    }
}

/// Persisted family-agency state: which scheduled tasks have been disabled
/// (kill switch) and which cron/interval tasks have been registered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgencyConfig {
    /// Identifiers of disabled tasks, as UUID strings.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Scheduled tasks registered by an agent or operator.
    #[serde(default)]
    pub scheduled_tasks: Vec<AgencyScheduledTask>,
}

impl AgencyConfig {
    /// Loads the config from a file. **A missing file means an empty
    /// config** (not an error) — this is normal on first startup.
    ///
    /// # Errors
    /// [`io::Error`] if the file exists but can't be read, or a
    /// [`serde_json`] parse error wrapped in an `io::Error`
    /// ([`io::ErrorKind::InvalidData`]) if the content isn't valid `JSON`.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Saves the config to a file (atomically: write to a temp file then
    /// rename, so an interrupted write never leaves a corrupt file behind).
    ///
    /// # Errors
    /// [`io::Error`] if creating the directory, writing, or renaming fails.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Atomic swap: write to an adjacent temp file, then rename over the target.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Whether the given task is marked disabled in this config.
    #[must_use]
    pub fn is_disabled(&self, id: ScheduledTaskId) -> bool {
        let id_str = id.to_string();
        self.disabled.iter().any(|d| d == &id_str)
    }

    /// Marks a task as disabled/enabled in the config (idempotent).
    ///
    /// `enabled = false` adds the id to the disabled list (if missing);
    /// `true` removes it. Does not write to disk — call [`save`](Self::save)
    /// separately.
    pub fn set(&mut self, id: ScheduledTaskId, enabled: bool) {
        let id_str = id.to_string();
        if enabled {
            self.disabled.retain(|d| d != &id_str);
        } else if !self.disabled.iter().any(|d| d == &id_str) {
            self.disabled.push(id_str);
        }
    }

    /// Adds or updates a scheduled task in the config (idempotent on `id`).
    pub fn upsert_scheduled_task(&mut self, task: AgencyScheduledTask) {
        if let Some(slot) = self
            .scheduled_tasks
            .iter_mut()
            .find(|entry| entry.id == task.id)
        {
            *slot = task;
        } else {
            self.scheduled_tasks.push(task);
        }
    }

    /// Registers the config's `scheduled_tasks` entries with the scheduler.
    ///
    /// Invalid entries are skipped silently (boot doesn't crash over one
    /// corrupt line). The `disabled` list is applied separately via
    /// [`Scheduler::apply_agency_config`].
    pub fn register_scheduled_tasks(&self, scheduler: &mut Scheduler) {
        for entry in &self.scheduled_tasks {
            match entry.to_scheduled_task() {
                Ok(task) => scheduler.register(task),
                Err(e) => tracing::warn!(
                    task_id = %entry.id,
                    error = %e,
                    "skipping invalid agency scheduled task"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> ScheduledTaskId {
        ScheduledTaskId::from_uuid(Uuid::from_u128(n))
    }

    #[test]
    fn default_is_empty() {
        assert!(AgencyConfig::default().disabled.is_empty());
    }

    #[test]
    fn set_toggles_idempotently() {
        let mut cfg = AgencyConfig::default();
        cfg.set(id(1), false);
        assert!(cfg.is_disabled(id(1)));
        // Repeated disable -> no duplicate.
        cfg.set(id(1), false);
        assert_eq!(cfg.disabled.len(), 1);
        // Re-enabling removes it.
        cfg.set(id(1), true);
        assert!(!cfg.is_disabled(id(1)));
        assert!(cfg.disabled.is_empty());
        // Re-enabling an unknown id -> no-op.
        cfg.set(id(2), true);
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn load_missing_file_is_empty_not_error() {
        let path = std::env::temp_dir().join("familyclaw-agency-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&path);
        let cfg = AgencyConfig::load(&path).expect("missing file → default");
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("familyclaw-agency-rt-test");
        let path = dir.join("agency.json");
        let _ = std::fs::remove_file(&path);

        let mut cfg = AgencyConfig::default();
        cfg.set(id(7), false);
        cfg.set(id(9), false);
        cfg.save(&path).expect("save");

        let loaded = AgencyConfig::load(&path).expect("load");
        assert!(loaded.is_disabled(id(7)));
        assert!(loaded.is_disabled(id(9)));
        assert!(!loaded.is_disabled(id(1)));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scheduled_task_roundtrips_to_scheduler_task() {
        let entry = AgencyScheduledTask {
            id: Uuid::from_u128(99).to_string(),
            skill_id: Uuid::from_u128(100).to_string(),
            payload: serde_json::json!({ "x": 1 }),
            cron_expression: Some("0 * * * *".to_string()),
            interval_secs: None,
            being_id: "agent_a".to_string(),
            enabled: true,
        };
        let task = entry.to_scheduled_task().expect("valid cron task");
        assert_eq!(task.cron_expression.as_deref(), Some("0 * * * *"));
        assert_eq!(task.being_id, "agent_a");
    }

    #[test]
    fn register_scheduled_tasks_applies_to_scheduler() {
        let mut scheduler = Scheduler::new();
        let mut cfg = AgencyConfig::default();
        cfg.scheduled_tasks.push(AgencyScheduledTask {
            id: Uuid::from_u128(200).to_string(),
            skill_id: Uuid::from_u128(201).to_string(),
            payload: serde_json::json!({}),
            cron_expression: Some("0 0 * * *".to_string()),
            interval_secs: None,
            being_id: "operator".to_string(),
            enabled: true,
        });
        cfg.register_scheduled_tasks(&mut scheduler);
        assert_eq!(scheduler.task_ids().len(), 1);
    }

    #[test]
    fn load_invalid_json_is_error() {
        let path = std::env::temp_dir().join("familyclaw-agency-bad.json");
        std::fs::write(&path, b"not json{{{").expect("write");
        let err = AgencyConfig::load(&path).expect_err("invalid → error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }
}

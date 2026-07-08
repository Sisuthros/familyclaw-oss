//! Perhe-agency-tilan **config-persistenssi** (Phase 4, D5).
//!
//! Ajastettujen tehtävien kill-switch-tila ([`crate::ScheduledTask::enabled`])
//! pitää säilyä yli prosessin restartin: jos operaattori pysäytti tehtävän, sen
//! pitää pysyä pysäytettynä myös uudelleenkäynnistyksen jälkeen. Tämä moduuli
//! tallentaa tilan **erilliseen config-tiedostoon** (esim.
//! `<data_dir>/agency.json`), EI durable-replay-journaliin — roadmap D5 varoittaa
//! nimenomaan timer-/agency-tilan sekoittamisesta replay-substraattiin
//! (eri elinkaari, eri omistajuus).
//!
//! Muoto on tarkoituksella minimaalinen: vain **disabloitujen tehtävien
//! id-lista** UUID-merkkijonoina. Käyttöön otetut tehtävät ovat oletus, joten
//! niitä ei tarvitse listata → tiedosto pysyy pienenä ja diffattavana.
//!
//! ```json
//! { "disabled": ["d4ea3c1c-0000-4000-8000-647265616d63"] }
//! ```
//!
//! I/O on **synkronista** (`std::fs`): tilaa luetaan kerran bootissa ja
//! kirjoitetaan vain harvoissa operaattorimutaatioissa, joten async-overhead ei
//! ole perusteltu. Puuttuva tiedosto = tyhjä config (ei virhe) — ensikäynnistys.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use chrono::Duration;
use familyclaw_actions::SkillId;

use crate::dispatch::Scheduler;
use crate::task::{ScheduledTask, ScheduledTaskId};

/// Persistoitu ajastettu tehtävä agency-configissa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgencyScheduledTask {
    /// Tehtävän vakaa tunniste (UUID-merkkijono).
    pub id: String,
    /// Suoritettavan taidon tunniste (UUID-merkkijono).
    pub skill_id: String,
    /// Taidolle annettava geneerinen JSON-payload.
    pub payload: Value,
    /// Cron-lauseke; kun asetettu, ohittaa `interval_secs`:n.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expression: Option<String>,
    /// Intervalli sekunteina (taaksepäin-yhteensopiva; käytetään jos ei cron).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<i64>,
    /// Olennon geneerinen tunniste lähetykselle.
    #[serde(default = "default_being_id")]
    pub being_id: String,
    /// Onko tehtävä aktiivinen (oletus `true`).
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
    /// Muuntaa config-merkinnän ajastimen [`ScheduledTask`]:ksi.
    ///
    /// Palauttaa virheen jos tunnisteet eivät ole kelvollisia UUID:ita tai
    /// aikataulua ei voi johtaa (ei cron eikä intervallia).
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

/// Persistoitu perhe-agency-tila: mitkä ajastetut tehtävät on otettu pois
/// käytöstä (kill-switch) ja mitkä cron/intervalli-tehtävät on rekisteröity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgencyConfig {
    /// Pois käytöstä otettujen tehtävien tunnisteet UUID-merkkijonoina.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Agentin tai operaattorin rekisteröimät ajastetut tehtävät.
    #[serde(default)]
    pub scheduled_tasks: Vec<AgencyScheduledTask>,
}

impl AgencyConfig {
    /// Lataa configin tiedostosta. **Puuttuva tiedosto = tyhjä config** (ei
    /// virhe) — tämä on normaali ensikäynnistys.
    ///
    /// # Errors
    /// [`io::Error`] jos tiedosto on olemassa mutta sitä ei voi lukea, tai
    /// [`serde_json`] -jäsennysvirhe käärittynä `io::Error`:iin
    /// ([`io::ErrorKind::InvalidData`]) jos sisältö ei ole kelvollista `JSON`ia.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Tallentaa configin tiedostoon (atominen: kirjoita temp + rename, jottei
    /// keskeytynyt kirjoitus jätä rikkinäistä tiedostoa).
    ///
    /// # Errors
    /// [`io::Error`] jos hakemiston luonti, kirjoitus tai uudelleennimeäminen
    /// epäonnistuu.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Atominen vaihto: kirjoita viereiseen temp-tiedostoon ja rename päälle.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Onko annettu tehtävä merkitty pois käytöstä tässä configissa.
    #[must_use]
    pub fn is_disabled(&self, id: ScheduledTaskId) -> bool {
        let id_str = id.to_string();
        self.disabled.iter().any(|d| d == &id_str)
    }

    /// Merkitsee tehtävän pois käytöstä / käyttöön configissa (idempotentti).
    ///
    /// `enabled = false` lisää id:n disabled-listaan (jos puuttuu); `true`
    /// poistaa sen. Ei kirjoita tiedostoon — kutsu [`save`](Self::save) erikseen.
    pub fn set(&mut self, id: ScheduledTaskId, enabled: bool) {
        let id_str = id.to_string();
        if enabled {
            self.disabled.retain(|d| d != &id_str);
        } else if !self.disabled.iter().any(|d| d == &id_str) {
            self.disabled.push(id_str);
        }
    }

    /// Lisää tai päivittää ajastetun tehtävän configissa (idempotentti `id`:llä).
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

    /// Rekisteröi configin `scheduled_tasks`-merkinnät ajastimeen.
    ///
    /// Virheelliset merkinnät ohitetaan hiljaisesti (boot ei kaadu yhden
    /// rikkinäisen rivin takia). `disabled`-lista sovelletaan erikseen
    /// [`Scheduler::apply_agency_config`]:lla.
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
        // Toista disablointi → ei duplikaattia.
        cfg.set(id(1), false);
        assert_eq!(cfg.disabled.len(), 1);
        // Käyttöön otto poistaa.
        cfg.set(id(1), true);
        assert!(!cfg.is_disabled(id(1)));
        assert!(cfg.disabled.is_empty());
        // Käyttöön otto tuntemattomalle → no-op.
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

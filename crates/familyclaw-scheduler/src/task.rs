//! Ajastetun tehtävän määrittely ([`ScheduledTask`]) ja sen tunniste
//! ([`ScheduledTaskId`]).
//!
//! Tehtävä on puhdas data-arvo: se kuvaa **mitä** taitoa ajetaan, **millä**
//! payloadilla, **kuinka usein** (intervalli) ja **kenen** nimissä. Tehtävä ei
//! itse suorita mitään — erääntyminen ([`crate::decision`]) ja lähetys
//! ([`crate::dispatch`]) ovat erillisiä.

use chrono::Duration;
use familyclaw_actions::SkillId;
use serde_json::Value;
use uuid::Uuid;

/// Ajastetun tehtävän vakaa tunniste.
///
/// Erillinen newtype [`Uuid`]-arvon päällä, jotta kääntäjä estää sekoittamisen
/// muihin tunnisteisiin. Tunniste on **vakaa**: se on osa idempotenssiavainta
/// ([`crate::decision::firing_key`]), joten saman loogisen tehtävän on
/// säilytettävä sama `ScheduledTaskId` yli prosessin restartin, jotta
/// kaatumiskestävä deduplikointi toimii.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScheduledTaskId(Uuid);

impl ScheduledTaskId {
    /// Luo uuden satunnaisen (`v4`) tunnisteen.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Kääri olemassa olevan [`Uuid`]-arvon tähän tunnistetyyppiin.
    ///
    /// Käytä tätä kun tunniste pitää johtaa vakaasti pysyvästä lähteestä
    /// (esim. konfiguraatiosta), jotta sama looginen tehtävä saa saman
    /// tunnisteen yli restartin.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Palauttaa sisällä olevan [`Uuid`]-arvon.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ScheduledTaskId {
    /// Oletuksena uusi satunnainen tunniste.
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ScheduledTaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Yksittäinen toistuvasti laukaistava työkalutehtävä.
///
/// Tehtävä laukeaa kun edellisestä laukaisusta on kulunut vähintään
/// [`ScheduledTask::interval`] (ks. [`crate::decision`]). Laukaisu reititetään
/// taidon ([`ScheduledTask::skill_id`]) ja payloadin
/// ([`ScheduledTask::payload`]) kanssa idempotentin lähetyksen läpi olennon
/// ([`ScheduledTask::being_id`]) nimissä.
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    /// Tehtävän vakaa tunniste (osa idempotenssiavainta).
    pub id: ScheduledTaskId,
    /// Suoritettavan taidon tunniste toimintopinon rekisterissä.
    pub skill_id: SkillId,
    /// Taidolle annettava payload (geneerinen JSON-arvo).
    pub payload: Value,
    /// Aikaväli laukaisujen välillä. Oletetaan positiiviseksi; ei-positiivinen
    /// intervalli kohdellaan "aina erääntyneenä" ([`crate::decision`]).
    pub interval: Duration,
    /// Olennon (being) geneerinen tunniste jonka nimissä lähetys tehdään
    /// (rate-limit-laskentaa varten toimintopinossa).
    pub being_id: String,
}

impl ScheduledTask {
    /// Rakentaa uuden ajastetun tehtävän satunnaisella tunnisteella.
    ///
    /// Käytä [`ScheduledTask::with_id`]:tä jos tarvitset vakaan, yli restartin
    /// säilyvän tunnisteen (kaatumiskestävä deduplikointi vaatii vakaan
    /// tunnisteen).
    #[must_use]
    pub fn new(
        skill_id: SkillId,
        payload: Value,
        interval: Duration,
        being_id: impl Into<String>,
    ) -> Self {
        Self {
            id: ScheduledTaskId::new(),
            skill_id,
            payload,
            interval,
            being_id: being_id.into(),
        }
    }

    /// Rakentaa uuden ajastetun tehtävän **annetulla vakaalla** tunnisteella.
    ///
    /// Tämä on suositeltu tapa tuotannossa: vakaa tunniste pitää
    /// idempotenssiavaimen samana yli prosessin restartin.
    #[must_use]
    pub fn with_id(
        id: ScheduledTaskId,
        skill_id: SkillId,
        payload: Value,
        interval: Duration,
        being_id: impl Into<String>,
    ) -> Self {
        Self {
            id,
            skill_id,
            payload,
            interval,
            being_id: being_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_displayable() {
        let a = ScheduledTaskId::new();
        let b = ScheduledTaskId::new();
        assert_ne!(a, b);
        assert_eq!(a.to_string(), a.as_uuid().to_string());
    }

    #[test]
    fn from_uuid_is_stable() {
        let raw = Uuid::from_u128(42);
        let id = ScheduledTaskId::from_uuid(raw);
        assert_eq!(id.as_uuid(), &raw);
        // Sama lähde-uuid → sama tunniste (vakaus yli restartin).
        assert_eq!(id, ScheduledTaskId::from_uuid(raw));
    }

    #[test]
    fn new_task_carries_fields() {
        let skill = SkillId::new();
        let task = ScheduledTask::new(skill, serde_json::json!({"k": 1}), Duration::seconds(60), "x");
        assert_eq!(task.skill_id, skill);
        assert_eq!(task.interval, Duration::seconds(60));
        assert_eq!(task.being_id, "x");
        assert_eq!(task.payload, serde_json::json!({"k": 1}));
    }
}

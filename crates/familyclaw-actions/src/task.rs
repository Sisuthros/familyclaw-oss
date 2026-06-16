//! Toimintotehtävä (action-task): toimintopinon ajettavan yksikön tila ja
//! elinkaari sekä jonot jotka säilyttävät tehtäviä (KERROS A, geneerinen).
//!
//! Tämä moduuli kattaa:
//! - [`TaskStatus`] — tehtävän tilakone ([`TaskStatus::can_transition_to`]
//!   koodaa lailliset siirtymät, [`TaskStatus::is_terminal`] päätelmät),
//! - [`ActionTask`] — yksittäinen toimintotehtävä rakennusvaiheittain
//!   ([`ActionTask::new`] + `with_*`-rakentajat, [`ActionTask::validate`]),
//! - [`TaskEvent`] — audit-tapahtumat tehtävän elinkaaresta,
//! - [`TaskQueue`] — in-memory-jono (tokio [`tokio::sync::Mutex`]),
//! - [`DurableTaskQueue`] — JSONL-tukeutuva jono joka liittää tilatilannekuvat
//!   (snapshot) tiedostoon ja osaa rekonstruoida tilan ([`DurableTaskQueue::reload`]).
//!
//! ## Determinismi
//! Puhdas tilakonelogiikka **ei lue kelloa**. Jonojen tilamuutosmetodit ottavat
//! aikaleiman injektoituna ([`familyclaw_core::time::Timestamp`]), jotta testit
//! ja durable-replay pysyvät deterministisinä.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use familyclaw_core::time::Timestamp;

use crate::error::{ActionError, Result};
use crate::ids::{ActionTaskId, ProofBundleId, SkillId};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Toimintotehtävän tila tilakoneessa.
///
/// Lailliset siirtymät koodataan [`TaskStatus::can_transition_to`]-metodissa.
/// Päätetilat ([`TaskStatus::Done`], [`TaskStatus::Failed`],
/// [`TaskStatus::Cancelled`]) eivät salli enää siirtymää eteenpäin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Suunniteltu: tehtävä on luotu mutta ei vielä valmis ajettavaksi.
    Planned,
    /// Valmis: tehtävä voidaan ottaa ajoon (riippuvuudet täyttyneet).
    Ready,
    /// Käynnissä: tehtävää suoritetaan parhaillaan.
    Running,
    /// Odottaa hyväksyntää: ihmisen hyväksyntä vaaditaan ennen jatkamista.
    NeedsApproval,
    /// Estetty: ulkoinen este (esim. riippuvuus) pysäyttää tehtävän.
    Blocked,
    /// Valmistunut onnistuneesti (päätetila).
    Done,
    /// Epäonnistui (päätetila).
    Failed,
    /// Peruutettu (päätetila).
    Cancelled,
}

impl TaskStatus {
    /// Onko tämä päätetila (ei enää siirtymiä eteenpäin).
    ///
    /// Päätetiloja ovat [`TaskStatus::Done`], [`TaskStatus::Failed`] ja
    /// [`TaskStatus::Cancelled`].
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Onko siirtymä tilasta `self` tilaan `next` laillinen.
    ///
    /// Sallitut reunat:
    /// - `Planned → Ready`
    /// - `Ready → Running`
    /// - `Running → {Done | Failed | NeedsApproval | Blocked}`
    /// - `NeedsApproval → Running`
    /// - `Blocked → Ready` (este poistui)
    /// - mikä tahansa **ei-päätetila** → `Cancelled`
    ///
    /// Päätetilat eivät salli mitään siirtymää. Siirtymä samaan tilaan ei ole
    /// sallittu (ei no-op-itsesiirtymiä).
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use TaskStatus::{
            Blocked, Cancelled, Done, Failed, NeedsApproval, Planned, Ready, Running,
        };

        // Päätetilasta ei voi siirtyä mihinkään.
        if self.is_terminal() {
            return false;
        }

        // Mikä tahansa ei-päätetila voidaan peruuttaa.
        if matches!(next, Cancelled) {
            return true;
        }

        matches!(
            (self, next),
            (Planned | Blocked, Ready)
                | (Ready | NeedsApproval, Running)
                | (Running, Done | Failed | NeedsApproval | Blocked)
        )
    }
}

/// Yksittäinen toimintotehtävä: ajettavan yksikön koko tila.
///
/// Tehtävä viittaa suoritettavaan taitoon ([`SkillId`]) ja kantaa payloadin
/// ([`serde_json::Value`]), uudelleenyrityslaskurin, ajastuksen sekä mahdollisen
/// todistepaketin tunnisteen. Aikaleimat ([`Timestamp`]) injektoidaan — niitä
/// ei lueta kellosta tämän tyypin sisällä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionTask {
    /// Tehtävän yksilöivä tunniste.
    pub id: ActionTaskId,
    /// Suoritettavan taidon tunniste.
    pub skill_id: SkillId,
    /// Tehtävän tila tilakoneessa.
    pub status: TaskStatus,
    /// Taidolle välitettävä syöte (geneerinen JSON).
    pub payload: serde_json::Value,
    /// Toteutuneiden uudelleenyritysten määrä.
    pub retry_count: u32,
    /// Aikaisin ajankohta jolloin tehtävän saa ottaa ajoon (`None` = heti).
    pub scheduled_at: Option<Timestamp>,
    /// Takaraja jonka jälkeen tehtävä on myöhässä (`None` = ei takarajaa).
    pub deadline: Option<Timestamp>,
    /// Suorituksesta syntyneen todistepaketin tunniste (`None` ennen suoritusta).
    pub proof_bundle_id: Option<ProofBundleId>,
    /// Luontihetki (injektoitu).
    pub created_at: Timestamp,
    /// Viimeisin päivityshetki (injektoitu).
    pub updated_at: Timestamp,
}

impl ActionTask {
    /// Luo uuden tehtävän tilassa [`TaskStatus::Planned`].
    ///
    /// Tunniste generoidaan satunnaisesti. Aikaleimat injektoidaan (`now`),
    /// ja sekä `created_at` että `updated_at` asetetaan samaksi.
    #[must_use]
    pub fn new(skill_id: SkillId, payload: serde_json::Value, now: Timestamp) -> Self {
        Self {
            id: ActionTaskId::new(),
            skill_id,
            status: TaskStatus::Planned,
            payload,
            retry_count: 0,
            scheduled_at: None,
            deadline: None,
            proof_bundle_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Rakentaja: asettaa eksplisiittisen tunnisteen.
    #[must_use]
    pub const fn with_id(mut self, id: ActionTaskId) -> Self {
        self.id = id;
        self
    }

    /// Rakentaja: asettaa aikaisimman ajoajan (`scheduled_at`).
    #[must_use]
    pub const fn with_scheduled_at(mut self, at: Timestamp) -> Self {
        self.scheduled_at = Some(at);
        self
    }

    /// Rakentaja: asettaa takarajan (`deadline`).
    #[must_use]
    pub const fn with_deadline(mut self, at: Timestamp) -> Self {
        self.deadline = Some(at);
        self
    }

    /// Rakentaja: liittää todistepaketin tunnisteen.
    #[must_use]
    pub const fn with_proof_bundle_id(mut self, id: ProofBundleId) -> Self {
        self.proof_bundle_id = Some(id);
        self
    }

    /// Validoi tehtävän sisäisen eheyden.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ManifestValidation`] jos:
    /// - tehtävän tai taidon tunniste on `nil`,
    /// - `updated_at` on ennen `created_at`-hetkeä,
    /// - takaraja (`deadline`) on ennen aikaisinta ajoaikaa (`scheduled_at`).
    pub fn validate(&self) -> Result<()> {
        if self.id.is_nil() {
            return Err(ActionError::ManifestValidation(
                "action task id puuttuu (nil)".to_string(),
            ));
        }
        if self.skill_id.is_nil() {
            return Err(ActionError::ManifestValidation(
                "skill id puuttuu (nil)".to_string(),
            ));
        }
        if self.updated_at < self.created_at {
            return Err(ActionError::ManifestValidation(
                "updated_at on ennen created_at-hetkeä".to_string(),
            ));
        }
        if let (Some(scheduled), Some(deadline)) = (self.scheduled_at, self.deadline) {
            if deadline < scheduled {
                return Err(ActionError::ManifestValidation(
                    "deadline on ennen scheduled_at-hetkeä".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Onko tehtävä valmis ajoon annetulla hetkellä `now`.
    ///
    /// Tehtävä on valmis kun tila on [`TaskStatus::Ready`] ja `scheduled_at`
    /// joko puuttuu tai on jo saavutettu (`scheduled_at <= now`).
    #[must_use]
    pub fn is_ready_at(&self, now: Timestamp) -> bool {
        self.status == TaskStatus::Ready && self.scheduled_at.is_none_or(|at| at <= now)
    }
}

/// Audit-tapahtuma tehtävän elinkaaresta.
///
/// Tapahtumat ovat tarkoitettu kirjattaviksi (audit-loki / durable-jono) ja ne
/// sarjallistuvat JSON-muotoon `kind`-erottelijalla.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    /// Tehtävä luotiin.
    Created {
        /// Tehtävän tunniste.
        task_id: ActionTaskId,
        /// Tila johon tehtävä luotiin (yleensä [`TaskStatus::Planned`]).
        status: TaskStatus,
        /// Tapahtuman hetki.
        at: Timestamp,
    },
    /// Tila vaihtui.
    StatusChanged {
        /// Tehtävän tunniste.
        task_id: ActionTaskId,
        /// Lähtötila.
        from: TaskStatus,
        /// Kohdetila.
        to: TaskStatus,
        /// Tapahtuman hetki.
        at: Timestamp,
    },
    /// Uudelleenyrityslaskuria kasvatettiin.
    RetryIncremented {
        /// Tehtävän tunniste.
        task_id: ActionTaskId,
        /// Uusi laskurin arvo kasvatuksen jälkeen.
        count: u32,
        /// Tapahtuman hetki.
        at: Timestamp,
    },
}

impl TaskEvent {
    /// Palauttaa tapahtumaan liittyvän tehtävän tunnisteen.
    #[must_use]
    pub const fn task_id(&self) -> ActionTaskId {
        match *self {
            Self::Created { task_id, .. }
            | Self::StatusChanged { task_id, .. }
            | Self::RetryIncremented { task_id, .. } => task_id,
        }
    }
}

/// In-memory-jono toimintotehtäthe operator.
///
/// Säilyttää tehtävät tunnisteen mukaan ja suojaa tilan tokio
/// [`tokio::sync::Mutex`]-lukolla, jotta jonoa voi jakaa async-tehtävien välillä.
/// Jono **ei lue kelloa** — kaikki tilamuutokset ottavat aikaleiman injektoituna.
#[derive(Debug, Default)]
pub struct TaskQueue {
    /// Tunniste → tehtävä -kartta lukon takana.
    inner: Mutex<HashMap<ActionTaskId, ActionTask>>,
}

impl TaskQueue {
    /// Luo uuden tyhjän jonon.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää tehtävän jonoon.
    ///
    /// Tehtävä validoidaan ([`ActionTask::validate`]) ennen tallennusta, ja
    /// saman tunnisteen kaksinkertainen lisäys hylätään.
    ///
    /// # Errors
    /// - Tehtävän validoinnin virhe.
    /// - [`ActionError::ManifestValidation`] jos sama tunniste on jo jonossa.
    pub async fn submit(&self, task: ActionTask) -> Result<()> {
        task.validate()?;
        let mut guard = self.inner.lock().await;
        if guard.contains_key(&task.id) {
            return Err(ActionError::ManifestValidation(format!(
                "tehtävä {} on jo jonossa (duplikaatti)",
                task.id
            )));
        }
        guard.insert(task.id, task);
        Ok(())
    }

    /// Hakee tehtävän tunnisteella (kopio); `None` jos ei löydy.
    pub async fn get(&self, id: ActionTaskId) -> Option<ActionTask> {
        self.inner.lock().await.get(&id).cloned()
    }

    /// Luettelee kaikki tehtävät (kopiot, järjestys määrittelemätön).
    pub async fn list(&self) -> Vec<ActionTask> {
        self.inner.lock().await.values().cloned().collect()
    }

    /// Luettelee tehtävät joiden tila vastaa annettua (kopiot).
    pub async fn list_by_status(&self, status: TaskStatus) -> Vec<ActionTask> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|t| t.status == status)
            .cloned()
            .collect()
    }

    /// Siirtää tehtävän uuteen tilaan jos siirtymä on laillinen.
    ///
    /// Päivittää myös `updated_at`-leiman injektoidulla hetkellä `now` ja
    /// palauttaa syntyneen [`TaskEvent::StatusChanged`]-tapahtuman.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] jos tehtävää ei ole jonossa.
    /// - [`ActionError::IllegalTransition`] jos siirtymä ei ole laillinen
    ///   (mukaan lukien yritys ajaa peruutettu tehtävä —
    ///   `Cancelled → Running` ei ole sallittu).
    pub async fn transition(
        &self,
        id: ActionTaskId,
        next: TaskStatus,
        now: Timestamp,
    ) -> Result<TaskEvent> {
        let mut guard = self.inner.lock().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {id} ei löydy")))?;
        let from = task.status;
        if !from.can_transition_to(next) {
            return Err(ActionError::IllegalTransition(format!(
                "{from:?} -> {next:?} ei ole sallittu (tehtävä {id})"
            )));
        }
        task.status = next;
        task.updated_at = now;
        Ok(TaskEvent::StatusChanged {
            task_id: id,
            from,
            to: next,
            at: now,
        })
    }

    /// Kasvattaa tehtävän uudelleenyrityslaskuria yhdellä.
    ///
    /// Päivittää `updated_at`-leiman ja palauttaa
    /// [`TaskEvent::RetryIncremented`]-tapahtuman.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] jos tehtävää ei ole jonossa.
    /// - [`ActionError::ExecutionFailed`] jos laskuri ylivuotaisi (`u32::MAX`).
    pub async fn increment_retry(&self, id: ActionTaskId, now: Timestamp) -> Result<TaskEvent> {
        let mut guard = self.inner.lock().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {id} ei löydy")))?;
        let next = task.retry_count.checked_add(1).ok_or_else(|| {
            ActionError::ExecutionFailed(format!("retry_count ylivuoto tehtävällä {id}"))
        })?;
        task.retry_count = next;
        task.updated_at = now;
        Ok(TaskEvent::RetryIncremented {
            task_id: id,
            count: next,
            at: now,
        })
    }

    /// Palauttaa ajettavissa olevan tehtävän annetulla hetkellä `now`.
    ///
    /// Valitsee tilan [`TaskStatus::Ready`] tehtävistä sen, jonka `scheduled_at`
    /// on jo saavutettu (tai puuttuu). Useasta ehdokkaasta valitaan deterministisesti
    /// pienimmän tunnisteen mukaan, jotta tulos on toistettava.
    pub async fn next_ready(&self, now: Timestamp) -> Option<ActionTask> {
        let guard = self.inner.lock().await;
        guard
            .values()
            .filter(|t| t.is_ready_at(now))
            .min_by_key(|t| *t.id.as_uuid())
            .cloned()
    }
}

/// JSONL-tukeutuva durable-jono toimintotehtäthe operator.
///
/// Jokainen tilamuutos kirjoitetaan yhtenä JSON-rivinä (`append`) tiedostoon:
/// rivi on tehtävän koko tilatilannekuva (snapshot). [`DurableTaskQueue::reload`]
/// lukee tiedoston ja rekonstruoi **viimeisimmän** tilan per tehtävätunniste
/// (myöhempi rivi voittaa). Toteutus on deterministinen: aikaleimat injektoidaan.
#[derive(Debug, Clone)]
pub struct DurableTaskQueue {
    /// JSONL-tiedoston polku johon tilannekuvat liitetään.
    path: PathBuf,
}

impl DurableTaskQueue {
    /// Luo durable-jonon annetulle tiedostopolulle.
    ///
    /// Tiedostoa ei luoda tässä; se syntyy ensimmäisellä [`DurableTaskQueue::append`]-
    /// kutsulla. Olemassa olevasta tiedostosta voi heti lukea
    /// [`DurableTaskQueue::reload`]-kutsulla.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Palauttaa tiedostopolun jota tämä jono käyttää.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Liittää tehtävän tilannekuvan (snapshot) JSONL-tiedostoon.
    ///
    /// Tehtävä validoidaan ennen kirjoitusta. Rivi on tehtävän koko tila
    /// JSON-muodossa, ja loppuun lisätään rivinvaihto.
    ///
    /// # Errors
    /// - Tehtävän validoinnin virhe.
    /// - [`ActionError::Proof`] jos sarjallistus tai tiedostokirjoitus epäonnistuu.
    pub async fn append(&self, task: &ActionTask) -> Result<()> {
        task.validate()?;
        let mut line = serde_json::to_string(task)
            .map_err(|e| ActionError::Proof(format!("snapshot serialize failed: {e}")))?;
        line.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| ActionError::Proof(format!("open durable file failed: {e}")))?;
        file.write_all(line.as_bytes())
            .await
            .map_err(|e| ActionError::Proof(format!("append durable line failed: {e}")))?;
        file.flush()
            .await
            .map_err(|e| ActionError::Proof(format!("flush durable file failed: {e}")))?;
        Ok(())
    }

    /// Rekonstruoi viimeisimmän tilan per tehtävätunniste JSONL-tiedostosta.
    ///
    /// Tyhjät rivit ohitetaan. Jokainen rivi on yksi tilannekuva; saman
    /// tunnisteen myöhempi rivi korvaa aiemman. Jos tiedostoa ei ole vielä
    /// olemassa, palautetaan tyhjä kartta (ei virhe).
    ///
    /// # Errors
    /// - [`ActionError::Proof`] jos tiedoston luku epäonnistuu (muu kuin
    ///   "ei löydy") tai jokin rivi ei ole kelvollinen tehtävä-JSON.
    pub async fn reload(&self) -> Result<HashMap<ActionTaskId, ActionTask>> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HashMap::new());
            }
            Err(e) => {
                return Err(ActionError::Proof(format!("read durable file failed: {e}")));
            }
        };
        let text = String::from_utf8(bytes)
            .map_err(|e| ActionError::Proof(format!("durable file not utf-8: {e}")))?;

        let mut latest: HashMap<ActionTaskId, ActionTask> = HashMap::new();
        for (lineno, raw) in text.lines().enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let task: ActionTask = serde_json::from_str(trimmed).map_err(|e| {
                ActionError::Proof(format!("rivin {} jäsennys epäonnistui: {e}", lineno + 1))
            })?;
            latest.insert(task.id, task);
        }
        Ok(latest)
    }

    /// Lataa durable-tiedostosta in-memory-jonon ([`TaskQueue`]).
    ///
    /// Kätevä apuri jolla durable-tilan saa takaisin ajettavaksi jonoksi.
    ///
    /// # Errors
    /// Sama kuin [`DurableTaskQueue::reload`].
    pub async fn load_into_queue(&self) -> Result<TaskQueue> {
        let map = self.reload().await?;
        Ok(TaskQueue {
            inner: Mutex::new(map),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;

    /// Apuri: kiinteä aikaleima determinististä testausta varten.
    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds in test")
    }

    /// Apuri: kelvollinen mock-tehtävä annetulla luontihetkellä.
    fn task_at(now: Timestamp) -> ActionTask {
        ActionTask::new(SkillId::new(), serde_json::json!({ "to": "general" }), now)
    }

    #[test]
    fn terminal_states_are_terminal() {
        assert!(TaskStatus::Done.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::Planned.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
    }

    #[test]
    fn legal_transitions_are_allowed() {
        assert!(TaskStatus::Planned.can_transition_to(TaskStatus::Ready));
        assert!(TaskStatus::Ready.can_transition_to(TaskStatus::Running));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Done));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Failed));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::NeedsApproval));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Blocked));
        assert!(TaskStatus::NeedsApproval.can_transition_to(TaskStatus::Running));
        assert!(TaskStatus::Blocked.can_transition_to(TaskStatus::Ready));
    }

    #[test]
    fn any_non_terminal_can_cancel() {
        assert!(TaskStatus::Planned.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::Ready.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::NeedsApproval.can_transition_to(TaskStatus::Cancelled));
        assert!(TaskStatus::Blocked.can_transition_to(TaskStatus::Cancelled));
    }

    #[test]
    fn terminal_states_cannot_transition() {
        for terminal in [TaskStatus::Done, TaskStatus::Failed, TaskStatus::Cancelled] {
            for next in [
                TaskStatus::Planned,
                TaskStatus::Ready,
                TaskStatus::Running,
                TaskStatus::NeedsApproval,
                TaskStatus::Blocked,
                TaskStatus::Done,
                TaskStatus::Failed,
                TaskStatus::Cancelled,
            ] {
                assert!(
                    !terminal.can_transition_to(next),
                    "{terminal:?} -> {next:?} pitäisi olla kielletty"
                );
            }
        }
    }

    #[test]
    fn illegal_jumps_are_rejected() {
        assert!(!TaskStatus::Planned.can_transition_to(TaskStatus::Running));
        assert!(!TaskStatus::Ready.can_transition_to(TaskStatus::Done));
        assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Ready));
        // Itsesiirtymät eivät ole sallittuja.
        assert!(!TaskStatus::Running.can_transition_to(TaskStatus::Running));
    }

    #[test]
    fn builders_and_validate() {
        let now = at(1_700_000_000);
        let task = task_at(now)
            .with_scheduled_at(at(1_700_000_100))
            .with_deadline(at(1_700_000_200))
            .with_proof_bundle_id(ProofBundleId::new());
        task.validate().expect("valid task validates");
        assert_eq!(task.scheduled_at, Some(at(1_700_000_100)));
        assert_eq!(task.deadline, Some(at(1_700_000_200)));
        assert!(task.proof_bundle_id.is_some());
    }

    #[test]
    fn validate_rejects_deadline_before_schedule() {
        let now = at(1_700_000_000);
        let task = task_at(now)
            .with_scheduled_at(at(1_700_000_200))
            .with_deadline(at(1_700_000_100));
        assert!(matches!(
            task.validate(),
            Err(ActionError::ManifestValidation(_))
        ));
    }

    #[test]
    fn validate_rejects_nil_ids() {
        let now = at(1_700_000_000);
        let task = task_at(now).with_id(ActionTaskId::nil());
        assert!(task.validate().is_err());
    }

    #[tokio::test]
    async fn happy_path_planned_ready_running_done() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;
        q.submit(task).await.expect("submit");

        let ev = q
            .transition(id, TaskStatus::Ready, at(1_700_000_001))
            .await
            .expect("planned->ready");
        assert!(matches!(
            ev,
            TaskEvent::StatusChanged {
                from: TaskStatus::Planned,
                to: TaskStatus::Ready,
                ..
            }
        ));

        q.transition(id, TaskStatus::Running, at(1_700_000_002))
            .await
            .expect("ready->running");
        q.transition(id, TaskStatus::Done, at(1_700_000_003))
            .await
            .expect("running->done");

        let final_task = q.get(id).await.expect("task present");
        assert_eq!(final_task.status, TaskStatus::Done);
        assert_eq!(final_task.updated_at, at(1_700_000_003));
    }

    #[tokio::test]
    async fn needs_approval_loop_back_to_running_then_done() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now);
        let id = task.id;
        q.submit(task).await.expect("submit");

        q.transition(id, TaskStatus::Ready, at(1))
            .await
            .expect("ready");
        q.transition(id, TaskStatus::Running, at(2))
            .await
            .expect("running");
        q.transition(id, TaskStatus::NeedsApproval, at(3))
            .await
            .expect("running->needs_approval");
        q.transition(id, TaskStatus::Running, at(4))
            .await
            .expect("needs_approval->running");
        q.transition(id, TaskStatus::Done, at(5))
            .await
            .expect("running->done");

        assert_eq!(q.get(id).await.expect("present").status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn running_failed_increments_retry_count() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now);
        let id = task.id;
        q.submit(task).await.expect("submit");

        q.transition(id, TaskStatus::Ready, at(1))
            .await
            .expect("ready");
        q.transition(id, TaskStatus::Running, at(2))
            .await
            .expect("running");
        q.transition(id, TaskStatus::Failed, at(3))
            .await
            .expect("running->failed");

        let ev = q.increment_retry(id, at(4)).await.expect("increment retry");
        assert!(matches!(ev, TaskEvent::RetryIncremented { count: 1, .. }));
        assert_eq!(q.get(id).await.expect("present").retry_count, 1);
    }

    #[tokio::test]
    async fn cancelled_task_cannot_run() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now);
        let id = task.id;
        q.submit(task).await.expect("submit");

        q.transition(id, TaskStatus::Cancelled, at(1))
            .await
            .expect("any non-terminal -> cancelled");

        let err = q
            .transition(id, TaskStatus::Running, at(2))
            .await
            .expect_err("cancelled task cannot run");
        assert!(matches!(err, ActionError::IllegalTransition(_)));
        assert_eq!(
            q.get(id).await.expect("present").status,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn duplicate_submit_rejected() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now);
        let id = task.id;
        q.submit(task.clone()).await.expect("first submit");
        assert!(q.submit(task).await.is_err());
        assert_eq!(q.get(id).await.expect("present").id, id);
    }

    #[tokio::test]
    async fn list_and_list_by_status() {
        let q = TaskQueue::new();
        let now = at(1_700_000_000);
        let t1 = task_at(now);
        let id1 = t1.id;
        let t2 = task_at(now);
        q.submit(t1).await.expect("submit t1");
        q.submit(t2).await.expect("submit t2");
        assert_eq!(q.list().await.len(), 2);

        q.transition(id1, TaskStatus::Ready, at(1))
            .await
            .expect("ready");
        let ready = q.list_by_status(TaskStatus::Ready).await;
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, id1);
    }

    #[tokio::test]
    async fn next_ready_honors_scheduled_at() {
        let q = TaskQueue::new();
        let base = at(1_700_000_000);

        // Tehtävä jolla on tuleva scheduled_at — ei vielä ajettavissa.
        let future = task_at(base).with_scheduled_at(at(1_700_001_000));
        let future_id = future.id;
        q.submit(future).await.expect("submit future");
        q.transition(future_id, TaskStatus::Ready, at(1))
            .await
            .expect("ready");

        // Hetkellä ennen scheduled_at: ei ajettavaa.
        assert!(q.next_ready(at(1_700_000_500)).await.is_none());

        // Tehtävä ilman scheduled_at — ajettavissa heti kun Ready.
        let nowable = task_at(base);
        let nowable_id = nowable.id;
        q.submit(nowable).await.expect("submit nowable");
        q.transition(nowable_id, TaskStatus::Ready, at(2))
            .await
            .expect("ready");

        let picked = q.next_ready(at(1_700_000_500)).await.expect("one ready");
        assert_eq!(picked.id, nowable_id);

        // Hetkellä scheduled_at jälkeen molemmat ovat ajettavissa.
        let later = q.next_ready(at(1_700_002_000)).await;
        assert!(later.is_some());
    }

    #[tokio::test]
    async fn missing_task_transition_is_not_found() {
        let q = TaskQueue::new();
        let err = q
            .transition(ActionTaskId::new(), TaskStatus::Ready, at(1))
            .await
            .expect_err("missing task");
        assert!(matches!(err, ActionError::NotFound(_)));
    }

    #[tokio::test]
    async fn durable_reload_preserves_state() {
        let dir = std::env::temp_dir();
        let unique = ActionTaskId::new();
        let path = dir.join(format!("familyclaw-actions-durable-{unique}.jsonl"));
        // Varmista puhdas lähtö.
        let _ = tokio::fs::remove_file(&path).await;

        let durable = DurableTaskQueue::new(&path);

        let now = at(1_700_000_000);
        let mut task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;

        // Kirjoita useita tilannekuvia: viimeisin (Running) jää voimaan.
        durable.append(&task).await.expect("append planned");

        task.status = TaskStatus::Ready;
        task.updated_at = at(1_700_000_001);
        durable.append(&task).await.expect("append ready");

        task.status = TaskStatus::Running;
        task.retry_count = 2;
        task.updated_at = at(1_700_000_002);
        durable.append(&task).await.expect("append running");

        // Toinen tehtävä samaan tiedostoon.
        let other = task_at(now).with_id(ActionTaskId::new());
        let other_id = other.id;
        durable.append(&other).await.expect("append other");

        // Uusi instanssi samaan polkuun: ei jaettua muistia.
        let reloaded = DurableTaskQueue::new(&path).reload().await.expect("reload");

        assert_eq!(reloaded.len(), 2);
        let restored = reloaded.get(&id).expect("first task restored");
        assert_eq!(restored.status, TaskStatus::Running);
        assert_eq!(restored.retry_count, 2);
        assert_eq!(restored.updated_at, at(1_700_000_002));
        assert!(reloaded.contains_key(&other_id));

        // load_into_queue palauttaa ajettavan jonon samasta tilasta.
        let queue = DurableTaskQueue::new(&path)
            .load_into_queue()
            .await
            .expect("load into queue");
        assert_eq!(
            queue.get(id).await.expect("present").status,
            TaskStatus::Running
        );

        // Siivous.
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn reload_missing_file_is_empty() {
        let path = std::env::temp_dir().join(format!(
            "familyclaw-actions-missing-{}.jsonl",
            ActionTaskId::new()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        let map = DurableTaskQueue::new(&path).reload().await.expect("reload");
        assert!(map.is_empty());
    }

    #[test]
    fn task_event_task_id_accessor() {
        let id = ActionTaskId::new();
        let ev = TaskEvent::Created {
            task_id: id,
            status: TaskStatus::Planned,
            at: at(1),
        };
        assert_eq!(ev.task_id(), id);
    }
}

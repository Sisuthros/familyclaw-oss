//! Tehtävätaulu: [`Task`], sen tilakone ([`TaskStatus`]) ja säieturvallinen
//! [`TaskBoard`].
//!
//! Taulu hoitaa tehtävien luonnin, tilasiirtymät ja luovutuksen (handoff)
//! agentilta toiselle. Tilakone on tarkoituksellisen tiukka: laittomat
//! siirtymät (esim. valmiin tehtävän uudelleenaktivointi) hylätään virheellä,
//! jotta durable-replay ja konsolidointi pysyvät johdonmukaisina.
//!
//! Kuten [`crate::agent`], taulu on riippumaton kuljetuskerroksesta ja
//! suojattu [`tokio::sync::RwLock`]illa.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::{FamilyClawError, Result};

/// Tehtävän vakaa tunniste.
///
/// Uudelleenkäyttää alustan [`MessageId`]-newtypeä (sama UUID-pohja, oma
/// nimi luettavuuden vuoksi).
pub type TaskId = MessageId;

/// Tehtävän tila (tilakone).
///
/// Sallitut siirtymät:
/// - `Pending → Active` (otetaan työn alle)
/// - `Pending → Handed`, `Active → Handed` (luovutus toiselle agentille)
/// - `Active → Done` (valmis)
/// - `Handed → Active` (vastaanottaja ottaa työn alle)
/// - `Pending → Done` (suora valmistuminen ilman erillistä aktivointia)
///
/// `Done` on terminaalinen — siitä ei ole siirtymiä eteenpäin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Luotu, ei vielä työn alla.
    Pending,
    /// Työn alla.
    Active,
    /// Valmis (terminaalinen).
    Done,
    /// Luovutettu odottamaan vastaanottajan kuittausta.
    Handed,
}

impl TaskStatus {
    /// Onko tila terminaalinen (ei siirtymiä eteenpäin).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done)
    }

    /// Onko siirtymä `self → next` sallittu.
    #[must_use]
    pub fn can_transition_to(self, next: TaskStatus) -> bool {
        use TaskStatus::{Active, Done, Handed, Pending};
        matches!(
            (self, next),
            (Pending, Active | Handed | Done) | (Active, Done | Handed) | (Handed, Active | Done)
        )
    }
}

/// Yksittäinen tehtävä taululla.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Tehtävän vakaa tunniste.
    pub id: TaskId,

    /// Lyhyt otsikko.
    pub title: String,

    /// Vapaamuotoinen kuvaus.
    #[serde(default)]
    pub description: String,

    /// Tehtävän nykyinen vastuuagentti, tai `None` jos jakamaton.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<AgentId>,

    /// Tehtävän nykyinen tila.
    pub status: TaskStatus,

    /// Luontihetki (UTC).
    pub created_at: Timestamp,

    /// Viimeisimmän muutoksen hetki (UTC).
    pub updated_at: Timestamp,
}

impl Task {
    /// Rakentaa uuden tehtävän tilassa [`TaskStatus::Pending`].
    ///
    /// `created_at` ja `updated_at` asetetaan nykyhetkeen.
    pub fn new(id: TaskId, title: impl Into<String>, assignee: Option<AgentId>) -> Self {
        let now = time::now();
        Self {
            id,
            title: title.into(),
            description: String::new(),
            assignee,
            status: TaskStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }

    /// Asettaa kuvauksen (builder-tyyli).
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Validoi tehtävän.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] jos otsikko on tyhjä.
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(FamilyClawError::invalid_input(
                "task title must not be empty",
            ));
        }
        Ok(())
    }
}

/// Säieturvallinen tehtävätaulu.
///
/// Hoitaa tehtävien luonnin, tilasiirtymät ja luovutuksen. Sisäinen tila on
/// suojattu [`tokio::sync::RwLock`]illa ja taulun voi kloonata (jaettu `Arc`).
#[derive(Debug, Clone, Default)]
pub struct TaskBoard {
    inner: Arc<RwLock<HashMap<TaskId, Task>>>,
}

impl TaskBoard {
    /// Luo tyhjän tehtävätaulun.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Luo uuden tehtävän taululle ja palauttaa sen.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] jos otsikko on tyhjä.
    pub async fn create(
        &self,
        title: impl Into<String>,
        assignee: Option<AgentId>,
    ) -> Result<Task> {
        let task = Task::new(TaskId::new(), title, assignee);
        task.validate()?;
        let mut guard = self.inner.write().await;
        guard.insert(task.id, task.clone());
        Ok(task)
    }

    /// Lisää valmiiksi rakennetun tehtävän taululle (esim. durable-replay).
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] jos tehtävä on kelvoton, tai
    /// jos taululla on jo sama tunniste.
    pub async fn insert(&self, task: Task) -> Result<()> {
        task.validate()?;
        let mut guard = self.inner.write().await;
        if guard.contains_key(&task.id) {
            return Err(FamilyClawError::invalid_input(format!(
                "task {} already exists",
                task.id
            )));
        }
        guard.insert(task.id, task);
        Ok(())
    }

    /// Hakee tehtävän tunnisteen perusteella.
    pub async fn get(&self, id: TaskId) -> Option<Task> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Tehtävien määrä taululla.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Onko taulu tyhjä.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }

    /// Palauttaa kaikki tehtävät luontihetken (ja tasapelitilanteessa
    /// tunnisteen) mukaan järjestettynä.
    pub async fn list(&self) -> Vec<Task> {
        let guard = self.inner.read().await;
        let mut out: Vec<Task> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        out
    }

    /// Palauttaa tietyn agentin vastuulla olevat tehtävät (luontijärjestys).
    pub async fn list_for_assignee(&self, assignee: AgentId) -> Vec<Task> {
        let guard = self.inner.read().await;
        let mut out: Vec<Task> = guard
            .values()
            .filter(|t| t.assignee == Some(assignee))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        out
    }

    /// Palauttaa tietyssä tilassa olevat tehtävät (luontijärjestys).
    pub async fn list_by_status(&self, status: TaskStatus) -> Vec<Task> {
        let guard = self.inner.read().await;
        let mut out: Vec<Task> = guard
            .values()
            .filter(|t| t.status == status)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        out
    }

    /// Vaihtaa tehtävän tilan tilakoneen sääntöjen mukaisesti ja palauttaa
    /// päivitetyn tehtävän.
    ///
    /// Sama-tilaan-siirtymä (`status == next`) on no-op joka onnistuu mutta
    /// ei muuta `updated_at`-leimaa.
    ///
    /// # Errors
    /// - [`FamilyClawError::NotFound`] jos tehtävää ei ole.
    /// - [`FamilyClawError::InvalidInput`] jos siirtymä on laiton.
    pub async fn update_status(&self, id: TaskId, next: TaskStatus) -> Result<Task> {
        let mut guard = self.inner.write().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| FamilyClawError::not_found(format!("task {id}")))?;

        if task.status == next {
            return Ok(task.clone());
        }
        if !task.status.can_transition_to(next) {
            return Err(FamilyClawError::invalid_input(format!(
                "illegal task transition: {:?} -> {:?}",
                task.status, next
            )));
        }
        task.status = next;
        task.updated_at = time::now();
        Ok(task.clone())
    }

    /// Luovuttaa tehtävän agentilta `from` agentille `to`.
    ///
    /// Säännöt:
    /// - Tehtävän nykyisen vastuuagentin on oltava `from` (estää väärinkäytön).
    /// - `from` ja `to` eivät saa olla sama agentti.
    /// - Tehtävä ei saa olla terminaalitilassa ([`TaskStatus::Done`]).
    ///
    /// Onnistuessa tehtävän `assignee` vaihtuu `to`:ksi ja tila siirtyy
    /// [`TaskStatus::Handed`]:iin. Palauttaa päivitetyn tehtävän.
    ///
    /// # Errors
    /// - [`FamilyClawError::NotFound`] jos tehtävää ei ole.
    /// - [`FamilyClawError::InvalidInput`] jos `from` ei ole nykyinen
    ///   vastuuagentti, `from == to`, tai tehtävä on terminaalitilassa.
    pub async fn handoff(&self, id: TaskId, from: AgentId, to: AgentId) -> Result<Task> {
        if from == to {
            return Err(FamilyClawError::invalid_input(
                "cannot hand off a task to the same agent",
            ));
        }
        let mut guard = self.inner.write().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| FamilyClawError::not_found(format!("task {id}")))?;

        if task.status.is_terminal() {
            return Err(FamilyClawError::invalid_input(format!(
                "cannot hand off a task in terminal status {:?}",
                task.status
            )));
        }
        match task.assignee {
            Some(current) if current == from => {}
            Some(current) => {
                return Err(FamilyClawError::invalid_input(format!(
                    "handoff source {from} is not current assignee {current}"
                )));
            }
            None => {
                return Err(FamilyClawError::invalid_input(format!(
                    "task {id} has no assignee; cannot hand off from {from}"
                )));
            }
        }

        task.assignee = Some(to);
        task.status = TaskStatus::Handed;
        task.updated_at = time::now();
        Ok(task.clone())
    }

    /// Asettaa tehtävän vastuuagentin (tai poistaa sen `None`:lla) muuttamatta
    /// tilaa. Palauttaa päivitetyn tehtävän.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos tehtävää ei ole.
    pub async fn assign(&self, id: TaskId, assignee: Option<AgentId>) -> Result<Task> {
        let mut guard = self.inner.write().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| FamilyClawError::not_found(format!("task {id}")))?;
        if task.assignee != assignee {
            task.assignee = assignee;
            task.updated_at = time::now();
        }
        Ok(task.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_transition_matrix() {
        use TaskStatus::{Active, Done, Handed, Pending};
        assert!(Pending.can_transition_to(Active));
        assert!(Pending.can_transition_to(Handed));
        assert!(Pending.can_transition_to(Done));
        assert!(Active.can_transition_to(Done));
        assert!(Active.can_transition_to(Handed));
        assert!(Handed.can_transition_to(Active));
        assert!(Handed.can_transition_to(Done));

        // Laittomat.
        assert!(!Done.can_transition_to(Active));
        assert!(!Done.can_transition_to(Pending));
        assert!(!Done.can_transition_to(Handed));
        assert!(!Active.can_transition_to(Pending));
        assert!(!Handed.can_transition_to(Pending));
    }

    #[test]
    fn done_is_terminal() {
        assert!(TaskStatus::Done.is_terminal());
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::Active.is_terminal());
        assert!(!TaskStatus::Handed.is_terminal());
    }

    #[test]
    fn task_new_defaults_to_pending() {
        let t = Task::new(TaskId::new(), "title", None);
        assert_eq!(t.status, TaskStatus::Pending);
        assert!(t.assignee.is_none());
        assert_eq!(t.created_at, t.updated_at);
    }

    #[test]
    fn task_validate_rejects_empty_title() {
        let t = Task::new(TaskId::new(), "   ", None);
        assert!(t.validate().is_err());
        let ok = Task::new(TaskId::new(), "ok", None);
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn task_serde_roundtrip() {
        let t = Task::new(TaskId::new(), "title", Some(AgentId::new()))
            .with_description("a description");
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Task = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[tokio::test]
    async fn create_and_get() {
        let board = TaskBoard::new();
        assert!(board.is_empty().await);
        let task = board.create("first", None).await.expect("create");
        assert_eq!(board.len().await, 1);
        let fetched = board.get(task.id).await.expect("present");
        assert_eq!(fetched, task);
    }

    #[tokio::test]
    async fn create_rejects_empty_title() {
        let board = TaskBoard::new();
        let err = board.create("  ", None).await.expect_err("empty title");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        assert!(board.is_empty().await);
    }

    #[tokio::test]
    async fn insert_rejects_duplicate_id() {
        let board = TaskBoard::new();
        let task = Task::new(TaskId::new(), "t", None);
        board.insert(task.clone()).await.expect("insert");
        let err = board.insert(task).await.expect_err("dup id");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn update_status_legal_and_illegal() {
        let board = TaskBoard::new();
        let task = board.create("t", None).await.expect("create");

        let active = board
            .update_status(task.id, TaskStatus::Active)
            .await
            .expect("pending->active");
        assert_eq!(active.status, TaskStatus::Active);
        assert!(active.updated_at >= task.updated_at);

        let done = board
            .update_status(task.id, TaskStatus::Done)
            .await
            .expect("active->done");
        assert_eq!(done.status, TaskStatus::Done);

        // Done on terminaalinen.
        let err = board
            .update_status(task.id, TaskStatus::Active)
            .await
            .expect_err("done->active illegal");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn update_status_same_state_is_noop() {
        let board = TaskBoard::new();
        let task = board.create("t", None).await.expect("create");
        let same = board
            .update_status(task.id, TaskStatus::Pending)
            .await
            .expect("noop");
        assert_eq!(same.status, TaskStatus::Pending);
        // updated_at ei muuttunut.
        assert_eq!(same.updated_at, task.updated_at);
    }

    #[tokio::test]
    async fn update_status_unknown_task_errors() {
        let board = TaskBoard::new();
        let err = board
            .update_status(TaskId::new(), TaskStatus::Active)
            .await
            .expect_err("unknown");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn handoff_happy_path() {
        let board = TaskBoard::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = board.create("t", Some(from)).await.expect("create");

        let handed = board.handoff(task.id, from, to).await.expect("handoff");
        assert_eq!(handed.assignee, Some(to));
        assert_eq!(handed.status, TaskStatus::Handed);

        // Vastaanottaja ottaa työn alle.
        let active = board
            .update_status(task.id, TaskStatus::Active)
            .await
            .expect("handed->active");
        assert_eq!(active.status, TaskStatus::Active);
        assert_eq!(active.assignee, Some(to));
    }

    #[tokio::test]
    async fn handoff_from_active_task() {
        let board = TaskBoard::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = board.create("t", Some(from)).await.expect("create");
        board
            .update_status(task.id, TaskStatus::Active)
            .await
            .expect("activate");

        let handed = board.handoff(task.id, from, to).await.expect("handoff");
        assert_eq!(handed.status, TaskStatus::Handed);
        assert_eq!(handed.assignee, Some(to));
    }

    #[tokio::test]
    async fn handoff_rejects_same_agent() {
        let board = TaskBoard::new();
        let a = AgentId::new();
        let task = board.create("t", Some(a)).await.expect("create");
        let err = board.handoff(task.id, a, a).await.expect_err("same agent");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn handoff_rejects_wrong_source() {
        let board = TaskBoard::new();
        let owner = AgentId::new();
        let imposter = AgentId::new();
        let to = AgentId::new();
        let task = board.create("t", Some(owner)).await.expect("create");
        let err = board
            .handoff(task.id, imposter, to)
            .await
            .expect_err("wrong source");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        // Tehtävä ei muuttunut.
        let unchanged = board.get(task.id).await.expect("present");
        assert_eq!(unchanged.assignee, Some(owner));
        assert_eq!(unchanged.status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn handoff_rejects_unassigned_task() {
        let board = TaskBoard::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = board.create("t", None).await.expect("create");
        let err = board
            .handoff(task.id, from, to)
            .await
            .expect_err("no assignee");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn handoff_rejects_terminal_task() {
        let board = TaskBoard::new();
        let from = AgentId::new();
        let to = AgentId::new();
        let task = board.create("t", Some(from)).await.expect("create");
        board
            .update_status(task.id, TaskStatus::Done)
            .await
            .expect("complete");
        let err = board
            .handoff(task.id, from, to)
            .await
            .expect_err("terminal");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn handoff_unknown_task_errors() {
        let board = TaskBoard::new();
        let err = board
            .handoff(TaskId::new(), AgentId::new(), AgentId::new())
            .await
            .expect_err("unknown");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn assign_changes_owner_without_status() {
        let board = TaskBoard::new();
        let a = AgentId::new();
        let task = board.create("t", None).await.expect("create");
        let assigned = board.assign(task.id, Some(a)).await.expect("assign");
        assert_eq!(assigned.assignee, Some(a));
        assert_eq!(assigned.status, TaskStatus::Pending);

        let cleared = board.assign(task.id, None).await.expect("clear");
        assert!(cleared.assignee.is_none());
    }

    #[tokio::test]
    async fn list_filters_and_order() {
        let board = TaskBoard::new();
        let a = AgentId::new();
        let t1 = board.create("t1", Some(a)).await.expect("t1");
        let t2 = board.create("t2", None).await.expect("t2");
        board
            .update_status(t2.id, TaskStatus::Active)
            .await
            .expect("activate t2");

        let all = board.list().await;
        assert_eq!(all.len(), 2);

        let for_a = board.list_for_assignee(a).await;
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].id, t1.id);

        let pending = board.list_by_status(TaskStatus::Pending).await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, t1.id);

        let active = board.list_by_status(TaskStatus::Active).await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, t2.id);
    }

    #[tokio::test]
    async fn board_clone_shares_state() {
        let board = TaskBoard::new();
        let clone = board.clone();
        let task = board.create("t", None).await.expect("create");
        assert!(clone.get(task.id).await.is_some());
    }
}

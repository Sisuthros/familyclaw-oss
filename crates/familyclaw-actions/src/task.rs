//! Action task: the state and lifecycle of a runnable unit of the action stack,
//! plus the queues that hold tasks (Layer A, generic).
//!
//! This module covers:
//! - [`TaskStatus`] — the task's state machine ([`TaskStatus::can_transition_to`]
//!   encodes the legal transitions, [`TaskStatus::is_terminal`] the terminal check),
//! - [`ActionTask`] — a single action task built step by step
//!   ([`ActionTask::new`] + `with_*` builders, [`ActionTask::validate`]),
//! - [`TaskEvent`] — audit events from the task's lifecycle,
//! - [`TaskQueue`] — an in-memory queue (tokio [`tokio::sync::Mutex`]),
//! - [`DurableTaskQueue`] — a JSONL-backed queue that appends state snapshots
//!   to a file and can reconstruct state ([`DurableTaskQueue::reload`]).
//!
//! ## Determinism
//! The pure state-machine logic **never reads the clock**. The queues' state-
//! transition methods take the timestamp injected
//! ([`familyclaw_core::time::Timestamp`]), so tests and durable replay stay
//! deterministic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use familyclaw_core::time::Timestamp;

use crate::error::{ActionError, Result};
use crate::ids::{ActionTaskId, ProofBundleId, SkillId};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// The action task's state in the state machine.
///
/// Legal transitions are encoded in the [`TaskStatus::can_transition_to`]
/// method. Terminal states ([`TaskStatus::Done`], [`TaskStatus::Failed`],
/// [`TaskStatus::Cancelled`]) no longer allow any further transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Planned: the task has been created but is not yet ready to run.
    Planned,
    /// Ready: the task can be picked up for execution (dependencies satisfied).
    Ready,
    /// Running: the task is currently being executed.
    Running,
    /// Needs approval: human approval is required before continuing.
    NeedsApproval,
    /// Blocked: an external obstacle (e.g. a dependency) is stopping the task.
    Blocked,
    /// Suspended due to backpressure: a resource budget (e.g. a concurrency
    /// or rate limit) was not available, so the task was paused and
    /// persisted to disk. Resumes to [`TaskStatus::Ready`] once the budget
    /// frees up. Difference from [`TaskStatus::Blocked`]: `Blocked` = external
    /// obstacle, `Suspended` = internal resource constraint (backpressure).
    Suspended,
    /// Completed successfully (terminal state).
    Done,
    /// Failed (terminal state).
    Failed,
    /// Cancelled (terminal state).
    Cancelled,
}

impl TaskStatus {
    /// Whether this is a terminal state (no further transitions).
    ///
    /// The terminal states are [`TaskStatus::Done`], [`TaskStatus::Failed`]
    /// and [`TaskStatus::Cancelled`].
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }

    /// Whether the transition from state `self` to state `next` is legal.
    ///
    /// Allowed edges:
    /// - `Planned → Ready`
    /// - `Ready → Running`
    /// - `Running → {Done | Failed | NeedsApproval | Blocked | Suspended}`
    /// - `NeedsApproval → Running`
    /// - `Blocked → Ready` (obstacle cleared)
    /// - `Suspended → Ready` (resource budget freed up — backpressure resolved)
    /// - any **non-terminal state** → `Cancelled`
    ///
    /// Terminal states allow no transition at all. A transition to the same
    /// state is not allowed (no no-op self-transitions).
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        use TaskStatus::{
            Blocked, Cancelled, Done, Failed, NeedsApproval, Planned, Ready, Running, Suspended,
        };

        // A terminal state cannot transition anywhere.
        if self.is_terminal() {
            return false;
        }

        // Any non-terminal state can be cancelled.
        if matches!(next, Cancelled) {
            return true;
        }

        matches!(
            (self, next),
            (Planned | Blocked | Suspended, Ready)
                | (Ready | NeedsApproval, Running)
                | (Running, Done | Failed | NeedsApproval | Blocked | Suspended)
        )
    }
}

/// A single action task: the full state of a runnable unit.
///
/// The task references the skill to execute ([`SkillId`]) and carries the
/// payload ([`serde_json::Value`]), a retry counter, scheduling, and an
/// optional proof bundle identifier. Timestamps ([`Timestamp`]) are injected —
/// none are read from the clock inside this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionTask {
    /// The task's unique identifier.
    pub id: ActionTaskId,
    /// The identifier of the skill to execute.
    pub skill_id: SkillId,
    /// The task's state in the state machine.
    pub status: TaskStatus,
    /// The input passed to the skill (generic JSON).
    pub payload: serde_json::Value,
    /// The number of retries that have occurred.
    pub retry_count: u32,
    /// The earliest time at which the task may be picked up for execution (`None` = immediately).
    pub scheduled_at: Option<Timestamp>,
    /// The deadline after which the task is late (`None` = no deadline).
    pub deadline: Option<Timestamp>,
    /// The identifier of the proof bundle produced by execution (`None` before execution).
    pub proof_bundle_id: Option<ProofBundleId>,
    /// The human-readable reason for a backpressure suspension (`None` when not suspended).
    ///
    /// Set when the task transitions to [`TaskStatus::Suspended`] and cleared
    /// when it resumes to [`TaskStatus::Ready`]. This field persists in the
    /// [`DurableTaskQueue`] snapshot, so that on restart it is known why the
    /// task is suspended. **Must never contain secrets** — only a generic
    /// resource reason (e.g. a budget name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension_reason: Option<String>,
    /// Creation time (injected).
    pub created_at: Timestamp,
    /// Most recent update time (injected).
    pub updated_at: Timestamp,
}

impl ActionTask {
    /// Creates a new task in state [`TaskStatus::Planned`].
    ///
    /// The identifier is generated randomly. Timestamps are injected (`now`),
    /// and both `created_at` and `updated_at` are set to the same value.
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
            suspension_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Builder: sets an explicit identifier.
    #[must_use]
    pub const fn with_id(mut self, id: ActionTaskId) -> Self {
        self.id = id;
        self
    }

    /// Builder: sets the earliest scheduled time (`scheduled_at`).
    #[must_use]
    pub const fn with_scheduled_at(mut self, at: Timestamp) -> Self {
        self.scheduled_at = Some(at);
        self
    }

    /// Builder: sets the deadline (`deadline`).
    #[must_use]
    pub const fn with_deadline(mut self, at: Timestamp) -> Self {
        self.deadline = Some(at);
        self
    }

    /// Builder: attaches the proof bundle identifier.
    #[must_use]
    pub const fn with_proof_bundle_id(mut self, id: ProofBundleId) -> Self {
        self.proof_bundle_id = Some(id);
        self
    }

    /// Validates the task's internal integrity.
    ///
    /// # Errors
    /// Returns [`ActionError::ManifestValidation`] if:
    /// - the task or skill identifier is `nil`,
    /// - `updated_at` is before `created_at`,
    /// - the deadline is before the earliest scheduled time (`scheduled_at`).
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

    /// Whether the task is ready to run at the given time `now`.
    ///
    /// The task is ready when its state is [`TaskStatus::Ready`] and
    /// `scheduled_at` is either absent or already reached (`scheduled_at <= now`).
    #[must_use]
    pub fn is_ready_at(&self, now: Timestamp) -> bool {
        self.status == TaskStatus::Ready && self.scheduled_at.is_none_or(|at| at <= now)
    }
}

/// An audit event from a task's lifecycle.
///
/// Events are intended to be recorded (audit log / durable queue) and they
/// serialize to JSON with a `kind` discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    /// The task was created.
    Created {
        /// The task's identifier.
        task_id: ActionTaskId,
        /// The state the task was created in (usually [`TaskStatus::Planned`]).
        status: TaskStatus,
        /// The event's time.
        at: Timestamp,
    },
    /// The state changed.
    StatusChanged {
        /// The task's identifier.
        task_id: ActionTaskId,
        /// The source state.
        from: TaskStatus,
        /// The target state.
        to: TaskStatus,
        /// The event's time.
        at: Timestamp,
    },
    /// The retry counter was incremented.
    RetryIncremented {
        /// The task's identifier.
        task_id: ActionTaskId,
        /// The counter's new value after incrementing.
        count: u32,
        /// The event's time.
        at: Timestamp,
    },
}

impl TaskEvent {
    /// Returns the task identifier associated with the event.
    #[must_use]
    pub const fn task_id(&self) -> ActionTaskId {
        match *self {
            Self::Created { task_id, .. }
            | Self::StatusChanged { task_id, .. }
            | Self::RetryIncremented { task_id, .. } => task_id,
        }
    }
}

/// An in-memory queue for action tasks.
///
/// Holds tasks keyed by identifier and guards the state with a tokio
/// [`tokio::sync::Mutex`] lock, so the queue can be shared across async tasks.
/// The queue **never reads the clock** — all state transitions take the
/// timestamp injected.
#[derive(Debug, Default)]
pub struct TaskQueue {
    /// Identifier → task map behind the lock.
    inner: Mutex<HashMap<ActionTaskId, ActionTask>>,
}

impl TaskQueue {
    /// Creates a new empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the queue **already populated** from the given identifier→task
    /// map (e.g. the result of [`DurableTaskQueue::reload`]).
    ///
    /// This is the crash-resilience recovery path: on restart the queue is
    /// reconstructed from state read off disk, so that tasks awaiting
    /// approval ([`TaskStatus::NeedsApproval`]) are still runnable
    /// (`run_after_approval`). Tasks are not re-validated here — they were
    /// already validated at write time ([`DurableTaskQueue::append`]).
    #[must_use]
    pub fn from_map(tasks: HashMap<ActionTaskId, ActionTask>) -> Self {
        Self {
            inner: Mutex::new(tasks),
        }
    }

    /// Adds a task to the queue.
    ///
    /// The task is validated ([`ActionTask::validate`]) before storing, and
    /// a duplicate insert of the same identifier is rejected.
    ///
    /// # Errors
    /// - A task validation error.
    /// - [`ActionError::ManifestValidation`] if the same identifier is already in the queue.
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

    /// Looks up a task by identifier (a copy); `None` if not found.
    pub async fn get(&self, id: ActionTaskId) -> Option<ActionTask> {
        self.inner.lock().await.get(&id).cloned()
    }

    /// Lists all tasks (copies, order unspecified).
    pub async fn list(&self) -> Vec<ActionTask> {
        self.inner.lock().await.values().cloned().collect()
    }

    /// Lists tasks whose state matches the given one (copies).
    pub async fn list_by_status(&self, status: TaskStatus) -> Vec<ActionTask> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|t| t.status == status)
            .cloned()
            .collect()
    }

    /// Transitions the task to a new state if the transition is legal.
    ///
    /// Also updates the `updated_at` timestamp with the injected time `now`
    /// and returns the resulting [`TaskEvent::StatusChanged`] event.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - [`ActionError::IllegalTransition`] if the transition is not legal
    ///   (including an attempt to run a cancelled task —
    ///   `Cancelled → Running` is not allowed).
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

    /// Suspends a task due to backpressure: transitions it to
    /// [`TaskStatus::Suspended`] and stores the generic `reason`.
    ///
    /// The transition is checked by the [`TaskStatus::can_transition_to`]
    /// machine (only `Running → Suspended` is legal). `reason` must not
    /// contain secrets — only a resource reason (e.g. a budget name).
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - [`ActionError::IllegalTransition`] if the current state cannot be suspended.
    pub async fn suspend(
        &self,
        id: ActionTaskId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> Result<TaskEvent> {
        let mut guard = self.inner.lock().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {id} ei löydy")))?;
        let from = task.status;
        if !from.can_transition_to(TaskStatus::Suspended) {
            return Err(ActionError::IllegalTransition(format!(
                "{from:?} -> Suspended ei ole sallittu (tehtävä {id})"
            )));
        }
        task.status = TaskStatus::Suspended;
        task.suspension_reason = Some(reason.into());
        task.updated_at = now;
        Ok(TaskEvent::StatusChanged {
            task_id: id,
            from,
            to: TaskStatus::Suspended,
            at: now,
        })
    }

    /// Resumes a suspended task: transitions it from
    /// [`TaskStatus::Suspended`] back to [`TaskStatus::Ready`] and clears the
    /// suspension reason. Used when a resource budget frees up.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - [`ActionError::IllegalTransition`] if the task is not suspended.
    pub async fn resume(&self, id: ActionTaskId, now: Timestamp) -> Result<TaskEvent> {
        let mut guard = self.inner.lock().await;
        let task = guard
            .get_mut(&id)
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {id} ei löydy")))?;
        let from = task.status;
        if from != TaskStatus::Suspended {
            return Err(ActionError::IllegalTransition(format!(
                "{from:?} -> Ready (resume) vaatii Suspended-tilan (tehtävä {id})"
            )));
        }
        task.status = TaskStatus::Ready;
        task.suspension_reason = None;
        task.updated_at = now;
        Ok(TaskEvent::StatusChanged {
            task_id: id,
            from,
            to: TaskStatus::Ready,
            at: now,
        })
    }

    /// Increments the task's retry counter by one.
    ///
    /// Updates the `updated_at` timestamp and returns the
    /// [`TaskEvent::RetryIncremented`] event.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] if the task is not in the queue.
    /// - [`ActionError::ExecutionFailed`] if the counter would overflow (`u32::MAX`).
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

    /// Returns a runnable task at the given time `now`.
    ///
    /// Among tasks in state [`TaskStatus::Ready`], picks the one whose
    /// `scheduled_at` has already been reached (or is absent). When there are
    /// multiple candidates, the choice is deterministic by the smallest
    /// identifier, so the result is reproducible.
    pub async fn next_ready(&self, now: Timestamp) -> Option<ActionTask> {
        let guard = self.inner.lock().await;
        guard
            .values()
            .filter(|t| t.is_ready_at(now))
            .min_by_key(|t| *t.id.as_uuid())
            .cloned()
    }
}

/// A JSONL-backed durable queue for action tasks.
///
/// Every state change is written as one JSON line (`append`) to the file:
/// the line is the task's full state snapshot. [`DurableTaskQueue::reload`]
/// reads the file and reconstructs the **latest** state per task identifier
/// (a later line wins). The implementation is deterministic: timestamps are injected.
#[derive(Debug, Clone)]
pub struct DurableTaskQueue {
    /// The path of the JSONL file that snapshots are appended to.
    path: PathBuf,
}

impl DurableTaskQueue {
    /// Creates a durable queue for the given file path.
    ///
    /// The file is not created here; it comes into existence on the first
    /// [`DurableTaskQueue::append`] call. An existing file can be read
    /// immediately via [`DurableTaskQueue::reload`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the file path this queue uses.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends the task's state snapshot to the JSONL file.
    ///
    /// The task is validated before writing. The line is the task's full
    /// state in JSON form, with a newline appended.
    ///
    /// # Errors
    /// - A task validation error.
    /// - [`ActionError::Proof`] if serialization or the file write fails.
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

    /// Reconstructs the latest state per task identifier from the JSONL file.
    ///
    /// Empty lines are skipped. Each line is one snapshot; a later line for
    /// the same identifier replaces the earlier one. If the file does not
    /// yet exist, an empty map is returned (not an error).
    ///
    /// # Errors
    /// - [`ActionError::Proof`] if reading the file fails (other than
    ///   "not found") or some line is not valid task JSON.
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

    /// Loads an in-memory queue ([`TaskQueue`]) from the durable file.
    ///
    /// A convenient helper to turn durable state back into a runnable queue.
    ///
    /// # Errors
    /// Same as [`DurableTaskQueue::reload`].
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

    /// Helper: a fixed timestamp for deterministic testing.
    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds in test")
    }

    /// Helper: a valid mock task at the given creation time.
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
        // Self-transitions are not allowed.
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

        // A task with a future scheduled_at — not yet runnable.
        let future = task_at(base).with_scheduled_at(at(1_700_001_000));
        let future_id = future.id;
        q.submit(future).await.expect("submit future");
        q.transition(future_id, TaskStatus::Ready, at(1))
            .await
            .expect("ready");

        // At a time before scheduled_at: nothing runnable.
        assert!(q.next_ready(at(1_700_000_500)).await.is_none());

        // A task without scheduled_at — runnable as soon as it's Ready.
        let nowable = task_at(base);
        let nowable_id = nowable.id;
        q.submit(nowable).await.expect("submit nowable");
        q.transition(nowable_id, TaskStatus::Ready, at(2))
            .await
            .expect("ready");

        let picked = q.next_ready(at(1_700_000_500)).await.expect("one ready");
        assert_eq!(picked.id, nowable_id);

        // At a time after scheduled_at, both are runnable.
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
        // Ensure a clean starting state.
        let _ = tokio::fs::remove_file(&path).await;

        let durable = DurableTaskQueue::new(&path);

        let now = at(1_700_000_000);
        let mut task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;

        // Write several snapshots: the latest one (Running) remains in effect.
        durable.append(&task).await.expect("append planned");

        task.status = TaskStatus::Ready;
        task.updated_at = at(1_700_000_001);
        durable.append(&task).await.expect("append ready");

        task.status = TaskStatus::Running;
        task.retry_count = 2;
        task.updated_at = at(1_700_000_002);
        durable.append(&task).await.expect("append running");

        // A second task in the same file.
        let other = task_at(now).with_id(ActionTaskId::new());
        let other_id = other.id;
        durable.append(&other).await.expect("append other");

        // A new instance for the same path: no shared memory.
        let reloaded = DurableTaskQueue::new(&path).reload().await.expect("reload");

        assert_eq!(reloaded.len(), 2);
        let restored = reloaded.get(&id).expect("first task restored");
        assert_eq!(restored.status, TaskStatus::Running);
        assert_eq!(restored.retry_count, 2);
        assert_eq!(restored.updated_at, at(1_700_000_002));
        assert!(reloaded.contains_key(&other_id));

        // load_into_queue returns a runnable queue from the same state.
        let queue = DurableTaskQueue::new(&path)
            .load_into_queue()
            .await
            .expect("load into queue");
        assert_eq!(
            queue.get(id).await.expect("present").status,
            TaskStatus::Running
        );

        // Cleanup.
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

    // ---- Track 2: Suspended state + backpressure (suspend/resume) ----

    #[test]
    fn suspend_transitions_are_legal_only_from_running() {
        // Running -> Suspended is a legal backpressure suspension.
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Suspended));
        // Suspended -> Ready is a legal resumption.
        assert!(TaskStatus::Suspended.can_transition_to(TaskStatus::Ready));
        // Suspended -> Cancelled (a non-terminal state can always be cancelled).
        assert!(TaskStatus::Suspended.can_transition_to(TaskStatus::Cancelled));
        // Suspended must NOT jump directly to Running (must go through Ready).
        assert!(!TaskStatus::Suspended.can_transition_to(TaskStatus::Running));
        // Cannot suspend from a non-Running state.
        assert!(!TaskStatus::Ready.can_transition_to(TaskStatus::Suspended));
        assert!(!TaskStatus::Planned.can_transition_to(TaskStatus::Suspended));
    }

    #[tokio::test]
    async fn running_suspends_on_backpressure_then_resumes() {
        let queue = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;
        queue.submit(task).await.expect("submit");
        queue
            .transition(id, TaskStatus::Ready, at(1_700_000_001))
            .await
            .expect("ready");
        queue
            .transition(id, TaskStatus::Running, at(1_700_000_002))
            .await
            .expect("running");

        // Backpressure: suspend with a budget reason.
        queue
            .suspend(
                id,
                "per_skill_concurrency budget exhausted",
                at(1_700_000_003),
            )
            .await
            .expect("suspend");
        let suspended = queue.get(id).await.expect("present");
        assert_eq!(suspended.status, TaskStatus::Suspended);
        assert_eq!(
            suspended.suspension_reason.as_deref(),
            Some("per_skill_concurrency budget exhausted")
        );

        // Budget freed up: resume.
        queue.resume(id, at(1_700_000_004)).await.expect("resume");
        let resumed = queue.get(id).await.expect("present");
        assert_eq!(resumed.status, TaskStatus::Ready);
        assert_eq!(
            resumed.suspension_reason, None,
            "reason nollataan resumessa"
        );
    }

    #[tokio::test]
    async fn suspend_from_non_running_is_rejected() {
        let queue = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;
        queue.submit(task).await.expect("submit");
        // Cannot suspend from the Planned state.
        let err = queue.suspend(id, "nope", at(1_700_000_001)).await;
        assert!(matches!(err, Err(ActionError::IllegalTransition(_))));
    }

    #[tokio::test]
    async fn resume_requires_suspended_state() {
        let queue = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;
        queue.submit(task).await.expect("submit");
        // Cannot resume a task that is not suspended.
        let err = queue.resume(id, at(1_700_000_001)).await;
        assert!(matches!(err, Err(ActionError::IllegalTransition(_))));
    }

    #[tokio::test]
    async fn durable_reload_preserves_suspension_reason() {
        let path = std::env::temp_dir().join(format!(
            "familyclaw-actions-suspend-{}.jsonl",
            ActionTaskId::new()
        ));
        let _ = tokio::fs::remove_file(&path).await;

        let durable = DurableTaskQueue::new(&path);
        let now = at(1_700_000_000);
        let mut task = task_at(now).with_id(ActionTaskId::new());
        let id = task.id;

        task.status = TaskStatus::Suspended;
        task.suspension_reason = Some("api_rate_limit budget exhausted".to_string());
        task.updated_at = at(1_700_000_010);
        durable.append(&task).await.expect("append suspended");

        // Restart: the suspension reason survives in the snapshot.
        let reloaded = DurableTaskQueue::new(&path).reload().await.expect("reload");
        let restored = reloaded.get(&id).expect("restored");
        assert_eq!(restored.status, TaskStatus::Suspended);
        assert_eq!(
            restored.suspension_reason.as_deref(),
            Some("api_rate_limit budget exhausted")
        );

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn suspension_reason_carries_no_secret() {
        // It is the operator's responsibility to supply a secret-free reason;
        // this test documents the invariant and verifies that suspend/resume
        // itself does not leak the payload into the reason.
        let queue = TaskQueue::new();
        let now = at(1_700_000_000);
        let task = ActionTask::new(
            SkillId::new(),
            serde_json::json!({ "token": "sk-supersecret-value" }),
            now,
        )
        .with_id(ActionTaskId::new());
        let id = task.id;
        queue.submit(task).await.expect("submit");
        queue
            .transition(id, TaskStatus::Ready, at(1_700_000_001))
            .await
            .expect("ready");
        queue
            .transition(id, TaskStatus::Running, at(1_700_000_002))
            .await
            .expect("running");
        queue
            .suspend(id, "queue_length budget exhausted", at(1_700_000_003))
            .await
            .expect("suspend");
        let reason = queue
            .get(id)
            .await
            .expect("present")
            .suspension_reason
            .expect("reason set");
        assert!(
            !reason.contains("sk-supersecret-value"),
            "keskeytyssyy ei saa sisältää payloadin salaisuutta"
        );
    }
}

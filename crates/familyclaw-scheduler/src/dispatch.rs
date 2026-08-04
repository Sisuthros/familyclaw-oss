//! [`Scheduler`]: the set of scheduled tasks + dispatch of due ones.
//!
//! The scheduler **never executes tools itself**. Firing a due task is
//! routed through the existing **idempotent dispatch**
//! ([`familyclaw_actions::ActionRuntime::submit_task_idempotent`]) with a
//! deterministic key ([`crate::decision::firing_key`]). This way the same
//! logical firing is dispatched **at most once**, even across a process
//! crash — the entire at-most-once guarantee is reused from the action
//! stack; the scheduler doesn't reinvent it.
//!
//! The due decision is made by pure logic ([`crate::decision`]) with an
//! injected clock — this module just wires that up to dispatch and records
//! `last_fired` state so a task doesn't fire again before its next interval.

use std::collections::HashMap;

use familyclaw_actions::skills::{FileWriteAllowlisted, ShellExec};
use familyclaw_actions::{ActionRuntime, Result, SkillId, SubmitOutcome};
use familyclaw_core::time::Timestamp;
use serde_json::Value;

use crate::decision::{decide, due_tasks};
use crate::task::{ScheduledTask, ScheduledTaskId};

/// Whether a skill honours a **tool-level** `idempotency_key` in its payload.
///
/// Only the two side-effecting skills that read that field
/// ([`FileWriteAllowlisted`], [`ShellExec`]) qualify; every other skill gets
/// its payload passed through untouched.
fn supports_tool_idempotency(skill_id: SkillId) -> bool {
    skill_id == FileWriteAllowlisted::skill_id() || skill_id == ShellExec::skill_id()
}

/// Derives the **tool-level** idempotency key from the scheduler firing key.
///
/// The firing key is `schedule-{task_id}-{bucket}`
/// ([`crate::decision::firing_key`]); the tool-level store expects
/// `<scope>:<stable-id>`, so the same firing becomes
/// `schedule:{task_id}:{bucket}`. Derived from the *existing* key, so both
/// levels dedup on exactly the same window — the scheduler-level key itself
/// is left unchanged.
fn tool_idempotency_key(task_id: ScheduledTaskId, firing_key: &str) -> String {
    let bucket = firing_key.rsplit('-').next().unwrap_or(firing_key);
    format!("schedule:{task_id}:{bucket}")
}

/// Inserts `idempotency_key` into a payload object, wrapping a non-object
/// payload first (shouldn't happen for the skills above, which require an
/// object payload).
fn inject_idempotency_key(payload: Value, key: String) -> Value {
    let mut object = match payload {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("payload".to_string(), other);
            map
        }
    };
    object.insert("idempotency_key".to_string(), Value::String(key));
    Value::Object(object)
}

/// Dispatch summary for a single tick.
///
/// Reports how many tasks were due and dispatched on this tick, along with
/// per-firing outcomes (identifier + idempotency key + dispatch result).
/// A summary only — no secrets and no raw payload.
#[derive(Debug, Default)]
pub struct DispatchSummary {
    /// Tasks that were due and dispatched on this tick: identifier, key, outcome.
    pub fired: Vec<(ScheduledTaskId, String, SubmitOutcome)>,
}

impl DispatchSummary {
    /// The number of tasks fired on this tick.
    #[must_use]
    pub fn fired_count(&self) -> usize {
        self.fired.len()
    }
}

/// **Dispatch instruction** for a single due task: everything needed for
/// idempotent dispatch, detached from [`Scheduler`].
///
/// [`Scheduler::collect_due`] returns these **quickly, while holding the
/// lock** (pure due decision + copying task data). Dispatch
/// ([`ActionRuntime::submit_task_idempotent`]) can then be run **without
/// the lock**, so long dispatch I/O doesn't block operator-surface
/// mutations (e.g. [`Scheduler::set_task_enabled`]). Contains only generic
/// identifiers and the payload — no lock, no reference to the scheduler
/// (Layer A safe).
#[derive(Debug, Clone)]
pub struct DueDispatch {
    /// The due task's identifier (for recording `last_fired`).
    pub task_id: ScheduledTaskId,
    /// The deterministic idempotency key for this firing.
    pub key: String,
    /// Identifier of the being on whose behalf dispatch happens.
    pub being_id: String,
    /// Identifier of the skill to execute.
    pub skill_id: SkillId,
    /// The payload passed to the skill.
    pub payload: Value,
}

/// Interval-based scheduler: holds a set of scheduled tasks and their last
/// firing times, and dispatches due ones idempotently.
///
/// The `last_fired` state is kept in memory; crash-resistant idempotency
/// comes from the **dispatch outbox** (deterministic key), not from this
/// state. If `last_fired` is lost on restart, the same interval window
/// yields the same key, so the outbox prevents a duplicate firing.
#[derive(Debug, Default)]
pub struct Scheduler {
    tasks: Vec<ScheduledTask>,
    last_fired: HashMap<ScheduledTaskId, Timestamp>,
    /// The most recently recorded human activity (family-agency:
    /// expire-if-no-human, Phase 4). `None` = no human seen yet. Updated
    /// via [`Scheduler::note_human_activity`] when a human is active.
    last_human_activity: Option<Timestamp>,
}

impl Scheduler {
    /// Creates an empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a scheduled task.
    ///
    /// Re-registering the same identifier **replaces** the previous task
    /// definition but **preserves** its `last_fired` state, so the interval
    /// isn't accidentally reset.
    pub fn register(&mut self, task: ScheduledTask) {
        if let Some(slot) = self.tasks.iter_mut().find(|t| t.id == task.id) {
            *slot = task;
        } else {
            self.tasks.push(task);
        }
    }

    /// Toggles a task on/off (family-agency kill switch, Phase 4).
    ///
    /// Sets the [`ScheduledTask::enabled`] flag on the given task. `false` =
    /// the scheduler skips it on subsequent ticks; `true` = re-enables it.
    /// Returns `true` if the task was found and the state was set, `false`
    /// if the identifier isn't registered. Does NOT reset `last_fired`
    /// state (re-enabling continues the normal interval; it won't fire
    /// immediately unless already due).
    pub fn set_task_enabled(&mut self, id: ScheduledTaskId, enabled: bool) -> bool {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// The task's current enabled state (introspection), or `None` if unknown.
    #[must_use]
    pub fn task_enabled(&self, id: ScheduledTaskId) -> Option<bool> {
        self.tasks.iter().find(|t| t.id == id).map(|t| t.enabled)
    }

    /// Identifiers of registered tasks (introspection for the operator surface).
    #[must_use]
    pub fn task_ids(&self) -> Vec<ScheduledTaskId> {
        self.tasks.iter().map(|t| t.id).collect()
    }

    /// Applies a persisted family-agency config to registered tasks
    /// (Phase 4): tasks in the config's disabled list are disabled, the
    /// rest are left enabled. Call this **at boot**, after registration, so
    /// the operator's kill switch survives a restart. Unknown ids in the
    /// config are silently ignored.
    pub fn apply_agency_config(&mut self, config: &crate::persistence::AgencyConfig) {
        for task in &mut self.tasks {
            task.enabled = !config.is_disabled(task.id);
        }
    }

    /// Records human activity (family-agency: expire-if-no-human, Phase 4).
    ///
    /// Call this when a human is active (e.g. an incoming human message).
    /// Updates `last_human_activity` only forward (never backward), so an
    /// old timestamp doesn't overwrite a more recent one.
    /// `expire_after_idle` tasks stay awake as long as a human has been
    /// active within that time window.
    pub fn note_human_activity(&mut self, at: Timestamp) {
        match self.last_human_activity {
            Some(prev) if prev >= at => {}
            _ => self.last_human_activity = Some(at),
        }
    }

    /// The most recently recorded human activity (introspection), or `None`.
    #[must_use]
    pub fn last_human_activity(&self) -> Option<Timestamp> {
        self.last_human_activity
    }

    /// The number of registered tasks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether the scheduler is empty (no registered tasks).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Returns the task's last firing time (or `None`).
    #[must_use]
    pub fn last_fired(&self, id: ScheduledTaskId) -> Option<Timestamp> {
        self.last_fired.get(&id).copied()
    }

    /// Computes the tasks due at this point in time **without executing**
    /// anything (a pure view for testing and introspection).
    #[must_use]
    pub fn due_now(&self, now: Timestamp) -> Vec<crate::decision::DueDecision> {
        due_tasks(
            &self.tasks,
            |id| self.last_fired(id),
            self.last_human_activity,
            now,
        )
    }

    /// Collects the **dispatch instructions** ([`DueDispatch`]) for tasks
    /// due at this point in time, **without executing** anything (pure, no
    /// `await`).
    ///
    /// This is deliberately cheap and synchronous: the due decision
    /// ([`crate::decision`]) is computed, and the dispatch data for due
    /// tasks (key, being, skill, payload) is copied out of the scheduler.
    /// This lets the shared scheduler's ([`crate::runner::SchedulerHandle`])
    /// lock be **released** before the actual dispatch I/O, so a long
    /// dispatch doesn't block operator-surface mutations (e.g.
    /// [`Scheduler::set_task_enabled`]). After running dispatch, call
    /// [`Scheduler::record_fired`] for each successful firing.
    #[must_use]
    pub fn collect_due(&self, now: Timestamp) -> Vec<DueDispatch> {
        let mut out = Vec::new();
        for decision in self.due_now(now) {
            let Some(key) = decision.key else { continue };
            let Some(task) = self.tasks.iter().find(|t| t.id == decision.task_id) else {
                continue;
            };
            // Side-effecting skills also dedup at the tool level: hand them
            // the same firing window as an `idempotency_key` in the payload.
            let payload = if supports_tool_idempotency(task.skill_id) {
                let tool_key = tool_idempotency_key(task.id, &key);
                inject_idempotency_key(task.payload.clone(), tool_key)
            } else {
                task.payload.clone()
            };
            out.push(DueDispatch {
                task_id: task.id,
                key,
                being_id: task.being_id.clone(),
                skill_id: task.skill_id,
                payload,
            });
        }
        out
    }

    /// Records a task's firing time (`last_fired`) after a successful
    /// dispatch.
    ///
    /// Separated from [`Scheduler::collect_due`] so dispatch can run
    /// without the scheduler lock: collect due tasks under the lock ->
    /// release the lock -> dispatch -> briefly reacquire the lock and call
    /// this per successful firing. `now` is the same point in time at
    /// which due-checking was evaluated, so the interval window stays
    /// consistent.
    pub fn record_fired(&mut self, task_id: ScheduledTaskId, now: Timestamp) {
        self.last_fired.insert(task_id, now);
    }

    /// Runs a single tick: dispatches all due tasks idempotently and
    /// records their `last_fired` time.
    ///
    /// Each due task is routed through
    /// [`ActionRuntime::submit_task_idempotent`] with its deterministic key
    /// ([`crate::decision::firing_key`]) — the scheduler doesn't execute
    /// the tool itself. `last_fired` is only updated after a successful
    /// dispatch, so a transient error doesn't "consume" the interval: the
    /// task retries on the next tick (and the idempotency key prevents a
    /// duplicate firing if the side effect was already committed to the
    /// outbox).
    ///
    /// Internally this is [`Scheduler::collect_due`] + dispatch +
    /// [`Scheduler::record_fired`]. Use it when the scheduler is **not**
    /// behind a shared lock (e.g. [`crate::runner::SchedulerRunner::run`]);
    /// in the shared run path
    /// ([`crate::runner::SchedulerRunner::run_shared`]) those two are
    /// called separately so the lock isn't held across dispatch.
    ///
    /// # Errors
    /// Returns the first dispatch error ([`ActionRuntime::submit_task_idempotent`]).
    /// Firings that succeeded before that are already recorded in the
    /// summary and in `last_fired` state.
    pub async fn tick(
        &mut self,
        runtime: &mut ActionRuntime,
        now: Timestamp,
    ) -> Result<DispatchSummary> {
        let mut summary = DispatchSummary::default();

        // Same due-checking and dispatch logic as the shared path: collect
        // due tasks (pure), dispatch idempotently, record successes.
        for dispatch in self.collect_due(now) {
            let outcome = runtime
                .submit_task_idempotent(
                    &dispatch.key,
                    &dispatch.being_id,
                    dispatch.skill_id,
                    dispatch.payload,
                    now,
                )
                .await?;

            // Record last_fired only after a successful dispatch.
            self.record_fired(dispatch.task_id, now);
            summary
                .fired
                .push((dispatch.task_id, dispatch.key, outcome));
        }

        Ok(summary)
    }

    /// Exposes a single task's due decision for inspection (introspection).
    #[must_use]
    pub fn decision_for(
        &self,
        id: ScheduledTaskId,
        now: Timestamp,
    ) -> Option<crate::decision::DueDecision> {
        self.tasks
            .iter()
            .find(|t| t.id == id)
            .map(|task| decide(task, self.last_fired(id), self.last_human_activity, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use familyclaw_actions::skills::FsReadAllowlisted;
    use familyclaw_actions::SkillId;
    use serde_json::json;

    fn at(unix_secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(unix_secs).expect("valid unix seconds")
    }

    fn runtime_with_fs_read() -> ActionRuntime {
        let mut rt = ActionRuntime::new();
        rt.register_skill(FsReadAllowlisted::new())
            .expect("register fs-read skill");
        rt
    }

    // (3) Idempotent dispatch: firing the same task twice with the same
    //     key routes through the outbox -> side effect at most once.
    #[tokio::test]
    async fn second_tick_in_same_window_does_not_refire() {
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();

        // fs-read fails (empty allowlist) but dispatch still returns a
        // committed result to the outbox — enough to prove dedup.
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(11)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        );
        sched.register(task);

        // Tick 1: due (never fired) -> dispatched.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick 1");
        assert_eq!(s1.fired_count(), 1);

        // Tick 2 in the same window: NOT due (last_fired = 0, interval 60).
        let s2 = sched.tick(&mut rt, at(30)).await.expect("tick 2");
        assert_eq!(s2.fired_count(), 0);

        // Tick 3 in the next window: due again.
        let s3 = sched.tick(&mut rt, at(60)).await.expect("tick 3");
        assert_eq!(s3.fired_count(), 1);
    }

    #[tokio::test]
    async fn set_task_enabled_toggles_and_reports() {
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(20));
        let task = ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({}),
            Duration::seconds(60),
            "being",
        );
        sched.register(task);

        // Default enabled.
        assert_eq!(sched.task_enabled(id), Some(true));
        assert_eq!(sched.task_ids(), vec![id]);

        // Kill-switch off.
        assert!(sched.set_task_enabled(id, false), "known id -> true");
        assert_eq!(sched.task_enabled(id), Some(false));

        // Back on.
        assert!(sched.set_task_enabled(id, true));
        assert_eq!(sched.task_enabled(id), Some(true));

        // Unknown id -> false, no panic.
        let unknown = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(999));
        assert!(!sched.set_task_enabled(unknown, false));
        assert_eq!(sched.task_enabled(unknown), None);
    }

    #[tokio::test]
    async fn idle_task_sleeps_until_human_activity() {
        // Family-agency (Phase 4) end-to-end: a task with an idle cap
        // doesn't fire into an empty room, but wakes up when a human is active.
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(40)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        )
        .with_expire_after_idle(Duration::seconds(100));
        sched.register(task);

        // No human ever -> idle-expired -> doesn't fire even though due.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick idle");
        assert_eq!(s1.fired_count(), 0, "doesn't fire into an empty room");

        // Human active -> wakes up -> fires (same window, same due-check).
        sched.note_human_activity(at(10));
        let s2 = sched.tick(&mut rt, at(10)).await.expect("tick after human");
        assert_eq!(s2.fired_count(), 1, "wakes up when a human is present");

        // Human away for a long time (200s idle > 100s cap) -> goes quiet again.
        // (Next window at(120) so due-checking would otherwise be fine.)
        let s3 = sched.tick(&mut rt, at(220)).await.expect("tick idle again");
        assert_eq!(s3.fired_count(), 0, "goes quiet again once human is away");
    }

    #[tokio::test]
    async fn apply_agency_config_restores_disabled_state() {
        use crate::persistence::AgencyConfig;
        let mut sched = Scheduler::new();
        let a = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(30));
        let b = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(31));
        for id in [a, b] {
            sched.register(ScheduledTask::with_id(
                id,
                FsReadAllowlisted::skill_id(),
                json!({}),
                Duration::seconds(60),
                "being",
            ));
        }
        // The config disables only a (simulates a restart where a was stopped).
        let mut cfg = AgencyConfig::default();
        cfg.set(a, false);
        sched.apply_agency_config(&cfg);

        assert_eq!(sched.task_enabled(a), Some(false), "a came back disabled");
        assert_eq!(sched.task_enabled(b), Some(true), "b remained enabled");
    }

    #[tokio::test]
    async fn disabled_task_does_not_fire_until_reenabled() {
        // Family-agency (Phase 4) end-to-end: a disabled task dispatches
        // nothing on tick; re-enabling restores firing.
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(12)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        )
        .with_enabled(false);
        sched.register(task.clone());

        // Disabled -> tick dispatches nothing, even though otherwise due.
        let s1 = sched.tick(&mut rt, at(0)).await.expect("tick disabled");
        assert_eq!(s1.fired_count(), 0, "disabled task does not fire");

        // Re-enable (register replaces the definition) -> fires.
        sched.register(task.with_enabled(true));
        let s2 = sched.tick(&mut rt, at(0)).await.expect("tick enabled");
        assert_eq!(s2.fired_count(), 1, "re-enabling restores firing");
    }

    #[tokio::test]
    async fn restart_with_lost_last_fired_dedups_via_outbox_key() {
        // Same key in the same window: even though the scheduler "forgets"
        // last_fired (a new Scheduler = restart), the idempotency key is
        // the same -> the outbox returns the same result without running
        // the side effect again.
        let mut rt = runtime_with_fs_read();
        let task = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(22)),
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nope"}),
            Duration::seconds(60),
            "being",
        );

        let mut sched_a = Scheduler::new();
        sched_a.register(task.clone());
        let s_a = sched_a.tick(&mut rt, at(70)).await.expect("tick a");
        assert_eq!(s_a.fired_count(), 1);
        let key_a = s_a.fired[0].1.clone();

        // "Restart": a new scheduler, last_fired lost, same window [60,120).
        let mut sched_b = Scheduler::new();
        sched_b.register(task);
        let s_b = sched_b.tick(&mut rt, at(119)).await.expect("tick b");
        assert_eq!(s_b.fired_count(), 1);
        let key_b = s_b.fired[0].1.clone();

        // Same key -> outbox dedup (side effect at most once).
        assert_eq!(key_a, key_b);
    }

    #[tokio::test]
    async fn empty_scheduler_tick_fires_nothing() {
        let mut rt = ActionRuntime::new();
        let mut sched = Scheduler::new();
        let s = sched.tick(&mut rt, at(0)).await.expect("empty tick");
        assert_eq!(s.fired_count(), 0);
    }

    #[tokio::test]
    async fn register_replaces_definition_keeps_last_fired() {
        let mut rt = runtime_with_fs_read();
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(33));
        sched.register(ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({"path": "/a"}),
            Duration::seconds(60),
            "b",
        ));
        sched.tick(&mut rt, at(0)).await.expect("tick");
        assert_eq!(sched.last_fired(id), Some(at(0)));

        // Re-register the same id: last_fired is preserved, doesn't fire immediately again.
        sched.register(ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({"path": "/b"}),
            Duration::seconds(60),
            "b",
        ));
        assert_eq!(sched.last_fired(id), Some(at(0)));
        let s = sched.tick(&mut rt, at(30)).await.expect("tick");
        assert_eq!(s.fired_count(), 0);
    }

    #[test]
    fn shell_exec_dispatch_carries_tool_level_idempotency_key() {
        use familyclaw_actions::idempotency::IdempotencyStore;
        use familyclaw_actions::skills::ShellExec;

        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(55));
        sched.register(ScheduledTask::with_id(
            id,
            ShellExec::skill_id(),
            json!({"command": "echo hi"}),
            Duration::seconds(60),
            "being",
        ));

        // Window [120, 180) -> bucket 2.
        let due = sched.collect_due(at(150));
        assert_eq!(due.len(), 1);
        let dispatch = &due[0];

        // The scheduler-level key is untouched.
        assert_eq!(dispatch.key, format!("schedule-{id}-2"));

        // The payload gained the tool-level key for the same window.
        let injected = dispatch.payload["idempotency_key"]
            .as_str()
            .expect("idempotency_key injected");
        assert_eq!(injected, format!("schedule:{id}:2"));
        assert!(injected.starts_with("schedule:"));
        assert!(injected.contains(':'));

        // ...and it survives the tool-level store's normalization unchanged.
        assert_eq!(
            IdempotencyStore::normalize_key(injected).expect("valid tool-level key"),
            injected
        );

        // Original payload fields are preserved.
        assert_eq!(dispatch.payload["command"], "echo hi");
    }

    #[test]
    fn other_skills_payload_is_not_rewritten() {
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(56));
        sched.register(ScheduledTask::with_id(
            id,
            FsReadAllowlisted::skill_id(),
            json!({"path": "/nonexistent"}),
            Duration::seconds(60),
            "being",
        ));

        let due = sched.collect_due(at(0));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].payload, json!({"path": "/nonexistent"}));
    }

    #[test]
    fn unknown_skill_decision_still_pure() {
        // decision_for dispatches nothing — just a pure inspection.
        let mut sched = Scheduler::new();
        let id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(44));
        sched.register(ScheduledTask::with_id(
            id,
            SkillId::new(),
            json!({}),
            Duration::seconds(10),
            "b",
        ));
        let d = sched.decision_for(id, at(0)).expect("decision");
        assert!(d.due);
    }
}

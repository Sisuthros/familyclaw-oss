//! Definition of a scheduled task ([`ScheduledTask`]) and its identifier
//! ([`ScheduledTaskId`]).
//!
//! A task is a pure data value: it describes **what** skill to run, **with
//! what** payload, **how often** (interval), and **on whose behalf**. The
//! task doesn't execute anything itself — due-checking ([`crate::decision`])
//! and dispatch ([`crate::dispatch`]) are separate concerns.

use chrono::Duration;
use familyclaw_actions::SkillId;
use serde_json::Value;
use uuid::Uuid;

/// Stable identifier for a scheduled task.
///
/// A distinct newtype wrapping a [`Uuid`] value, so the compiler prevents
/// mixing it up with other identifiers. The identifier is **stable**: it's
/// part of the idempotency key ([`crate::decision::firing_key`]), so the same
/// logical task must keep the same `ScheduledTaskId` across a process
/// restart for crash-resistant deduplication to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScheduledTaskId(Uuid);

impl ScheduledTaskId {
    /// Creates a new random (`v4`) identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing [`Uuid`] value into this identifier type.
    ///
    /// Use this when the identifier needs to be derived deterministically
    /// from a persistent source (e.g. configuration), so the same logical
    /// task gets the same identifier across a restart.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Returns the wrapped [`Uuid`] value.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ScheduledTaskId {
    /// Defaults to a new random identifier.
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ScheduledTaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// A single, repeatedly-firing tool task.
///
/// The task fires once at least [`ScheduledTask::interval`] has elapsed
/// since the previous firing (see [`crate::decision`]). Firing is routed
/// through idempotent dispatch together with the skill
/// ([`ScheduledTask::skill_id`]) and payload ([`ScheduledTask::payload`]),
/// on behalf of the being ([`ScheduledTask::being_id`]).
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    /// The task's stable identifier (part of the idempotency key).
    pub id: ScheduledTaskId,
    /// Identifier of the skill to execute, in the action stack's registry.
    pub skill_id: SkillId,
    /// The payload passed to the skill (a generic JSON value).
    pub payload: Value,
    /// The interval between firings. Assumed to be positive; a
    /// non-positive interval is treated as "always due"
    /// (see [`crate::decision`]).
    ///
    /// Ignored when [`cron_expression`] is set — in that case, due-checking
    /// is derived from the cron expression instead.
    pub interval: Duration,
    /// Optional cron expression (e.g. `"0 * * * *"`). When set, the task
    /// fires according to the cron schedule ([`crate::decision`]); otherwise
    /// [`interval`] is used (backward-compatible default).
    pub cron_expression: Option<String>,
    /// Generic identifier of the being on whose behalf dispatch happens
    /// (used for rate-limit accounting in the action stack).
    pub being_id: String,
    /// **Family-agency control (Phase 4): whether the task is active.**
    ///
    /// `true` (default) = the task fires normally. `false` = the scheduler
    /// **skips** it ([`crate::decision::decide`] returns `due=false`) until
    /// it's re-enabled. This is a **kill switch / opt-in**: a human can stop
    /// a proactive scheduled task without shutting down the whole scheduler.
    /// This state is part of the task definition, so an operator surface can
    /// toggle it on/off and persist the choice.
    pub enabled: bool,
    /// **Family-agency control (Phase 4): expire-if-no-human.**
    ///
    /// If set, the task **stops firing** once more than this amount of time
    /// has passed since the last human activity ([`crate::decision::decide`]
    /// returns `due=false`). This prevents a proactive agent from continuing
    /// to act autonomously into an empty room: when no human is present,
    /// scheduled tasks go quiet on their own and wake up again once a human
    /// returns (activity refreshes -> no longer expired). `None` (default) =
    /// never expires due to idleness.
    pub expire_after_idle: Option<Duration>,
}

impl ScheduledTask {
    /// Builds a new scheduled task with a random identifier.
    ///
    /// Use [`ScheduledTask::with_id`] if you need a stable identifier that
    /// survives a restart (crash-resistant deduplication requires a stable
    /// identifier).
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
            cron_expression: None,
            being_id: being_id.into(),
            enabled: true,
            expire_after_idle: None,
        }
    }

    /// Builds a new scheduled task with a **given, stable** identifier.
    ///
    /// This is the recommended approach in production: a stable identifier
    /// keeps the idempotency key the same across a process restart.
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
            cron_expression: None,
            being_id: being_id.into(),
            enabled: true,
            expire_after_idle: None,
        }
    }

    /// Sets the cron expression and returns `self` for chaining.
    ///
    /// When set, [`crate::decision`] uses cron-based due-checking instead
    /// of [`ScheduledTask::interval`].
    #[must_use]
    pub fn with_cron_expression(mut self, cron_expression: impl Into<String>) -> Self {
        self.cron_expression = Some(cron_expression.into());
        self
    }

    /// Sets the [`ScheduledTask::enabled`] state (family-agency control).
    ///
    /// `with_enabled(false)` = kill switch: the scheduler skips the task
    /// until it's re-enabled. Returns `self` for chaining.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets [`ScheduledTask::expire_after_idle`] (family-agency:
    /// expire-if-no-human). The task stops firing once more than `idle` has
    /// passed since the last human activity; it wakes up again once a human
    /// returns. Returns `self` for chaining.
    #[must_use]
    pub const fn with_expire_after_idle(mut self, idle: Duration) -> Self {
        self.expire_after_idle = Some(idle);
        self
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
        // Same source uuid -> same identifier (stability across a restart).
        assert_eq!(id, ScheduledTaskId::from_uuid(raw));
    }

    #[test]
    fn new_task_carries_fields() {
        let skill = SkillId::new();
        let task = ScheduledTask::new(
            skill,
            serde_json::json!({"k": 1}),
            Duration::seconds(60),
            "x",
        );
        assert_eq!(task.skill_id, skill);
        assert_eq!(task.interval, Duration::seconds(60));
        assert_eq!(task.being_id, "x");
        assert_eq!(task.payload, serde_json::json!({"k": 1}));
    }
}

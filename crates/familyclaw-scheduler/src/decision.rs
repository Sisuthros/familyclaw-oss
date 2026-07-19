//! **Pure** due-checking and key-derivation logic (clock is injected).
//!
//! This module never reads the real clock: the current time is always
//! passed in as an injected [`Timestamp`]. That's why the entire logic —
//! *which* tasks are due and *what* idempotency key they fire with — is
//! unit-testable without a real clock. Only [`crate::runner`] touches
//! [`tokio::time`].
//!
//! ## Due rule
//! A task is **due** when
//! - **interval:** `now >= last_fired + interval` (default), or
//! - **cron:** the cron occurrence at or before `now` is more recent than
//!   `last_fired` (see [`is_due_cron`]).
//!
//! If the task has never fired (`last_fired = None`), it's due immediately
//! on the first evaluation (interval) or as soon as a cron occurrence has
//! been reached (cron). A non-positive interval is treated as "always due"
//! (a safe degenerate case; in production the interval is assumed to be
//! positive).
//!
//! ## Idempotency key stability
//! The firing key is `schedule-{task_id}-{epoch_bucket}` (interval) or
//! `schedule-{task_id}-{occurrence_unix}` (cron), where `occurrence_unix` is
//! the Unix timestamp of the cron occurrence. Within the same interval
//! window or cron occurrence, **any** `now` produces the same key.

use chrono::Duration;
use croner::Cron;
use familyclaw_core::time::Timestamp;
use std::str::FromStr;

use crate::task::{ScheduledTask, ScheduledTaskId};

/// The due decision for a single task at a given point in time.
///
/// Contains the task's identifier, its due state, and — if due — the
/// deterministic idempotency key derived for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDecision {
    /// The identifier of the task this decision refers to.
    pub task_id: ScheduledTaskId,
    /// Whether the task is due (will it fire for this `now` value).
    pub due: bool,
    /// The deterministic idempotency key for this firing, if `due == true`.
    ///
    /// The key is stable across a restart for the same logical firing
    /// window (see [`firing_key`]); for a task that isn't due, this is
    /// `None`.
    pub key: Option<String>,
}

/// Derives a **deterministic** idempotency key for a single firing.
///
/// The key is `schedule-{task_id}-{epoch_bucket}`, where
/// `epoch_bucket = floor(now_unix / interval_secs)`. Properties:
///
/// - **Stable within a window:** all `now` values within the same
///   `[bucket*interval, (bucket+1)*interval)` window produce the same key,
///   so a crash/restart re-evaluation lands on the key already committed
///   in the dispatch outbox instead of firing the side effect a second time.
/// - **Independent of process memory:** derived solely from `task_id`,
///   interval, and current time — not from tick counters that a restart
///   would reset.
/// - **Changes across windows:** the next interval window gets a new
///   bucket, so the next firing has a different key and won't incorrectly
///   deduplicate against the previous one.
///
/// A non-positive interval is treated as a one-second window for key
/// derivation, so the key stays well-defined even in the degenerate case
/// (due-checking itself is handled separately in [`is_due`]).
#[must_use]
pub fn firing_key(task_id: ScheduledTaskId, interval: Duration, now: Timestamp) -> String {
    let interval_secs = interval.num_seconds().max(1);
    let now_secs = now.timestamp();
    let bucket = now_secs.div_euclid(interval_secs);
    format!("schedule-{task_id}-{bucket}")
}

/// Parses a cron expression. Returns `None` for an invalid expression.
#[must_use]
pub fn parse_cron(expression: &str) -> Option<Cron> {
    Cron::from_str(expression).ok()
}

/// Returns the most recent cron occurrence at or before `now`.
#[must_use]
pub fn cron_occurrence_at(expression: &str, now: Timestamp) -> Option<Timestamp> {
    let cron = parse_cron(expression)?;
    cron.find_previous_occurrence(&now, true).ok()
}

/// Whether a cron task is due at the given point in time.
///
/// `last_fired = None` -> due as soon as a cron occurrence can be found.
/// Otherwise due when the most recent cron occurrence `<= now` is more
/// recent than `last_fired`. An invalid expression -> never due
/// (fail-closed).
#[must_use]
pub fn is_due_cron(expression: &str, last_fired: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(occurrence) = cron_occurrence_at(expression, now) else {
        return false;
    };
    match last_fired {
        None => true,
        Some(last) => last < occurrence,
    }
}

/// Derives a deterministic idempotency key for a single cron firing.
///
/// The key is `schedule-{task_id}-{occurrence_unix}`, where
/// `occurrence_unix` is the occurrence returned by [`cron_occurrence_at`].
/// An invalid expression returns a key with the `invalid` suffix (a task
/// that isn't due never uses it).
#[must_use]
pub fn cron_firing_key(task_id: ScheduledTaskId, expression: &str, now: Timestamp) -> String {
    match cron_occurrence_at(expression, now) {
        Some(occurrence) => format!("schedule-{task_id}-{}", occurrence.timestamp()),
        None => format!("schedule-{task_id}-invalid"),
    }
}

/// Whether a task is due at the given point in time.
///
/// `last_fired = None` means "never fired" -> due immediately. Otherwise
/// due when `now >= last_fired + interval`. A non-positive interval ->
/// always due.
#[must_use]
pub fn is_due(interval: Duration, last_fired: Option<Timestamp>, now: Timestamp) -> bool {
    if interval <= Duration::zero() {
        return true;
    }
    match last_fired {
        None => true,
        Some(last) => now >= last + interval,
    }
}

/// Evaluates the due decision ([`DueDecision`]) for a single task.
///
/// A pure function: no side effects, clock injected. If the task is due,
/// the returned decision includes the deterministic key derived for it
/// ([`firing_key`]).
#[must_use]
pub fn decide(
    task: &ScheduledTask,
    last_fired: Option<Timestamp>,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> DueDecision {
    // Family-agency (Phase 4): a disabled task NEVER fires — the human
    // kill-switch overrides due-checking entirely. AND: expire-if-no-human —
    // if an idle cap is set and too much time has passed since human
    // activity, the task goes quiet (doesn't fire) until a human returns.
    let schedule_due = if let Some(ref cron) = task.cron_expression {
        is_due_cron(cron, last_fired, now)
    } else {
        is_due(task.interval, last_fired, now)
    };
    let due = task.enabled
        && !idle_expired(task.expire_after_idle, last_human_activity, now)
        && schedule_due;
    let key = if due {
        if let Some(ref cron) = task.cron_expression {
            Some(cron_firing_key(task.id, cron, now))
        } else {
            Some(firing_key(task.id, task.interval, now))
        }
    } else {
        None
    };
    DueDecision {
        task_id: task.id,
        due,
        key,
    }
}

/// Whether a task has **expired due to idleness** (family-agency:
/// expire-if-no-human).
///
/// `expire_after_idle = None` -> never expires (returns `false`). Otherwise
/// expired when `now - last_human_activity > expire_after_idle`. If human
/// activity has never been recorded (`None`), the task is expired
/// immediately whenever an idle cap is set — a proactive task won't start
/// up in an empty room before a human has been present at least once.
#[must_use]
pub fn idle_expired(
    expire_after_idle: Option<Duration>,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    let Some(idle) = expire_after_idle else {
        return false; // no idle cap -> never expires
    };
    if idle <= Duration::zero() {
        return false; // non-positive cap -> never expires (safe degenerate case)
    }
    match last_human_activity {
        None => true, // no human ever + idle cap set -> expired
        Some(last) => now > last + idle,
    }
}

/// Computes the due tasks for a list in one pass.
///
/// `last_fired` is a lookup function that returns a task's last firing time
/// (or `None` if it never fired). `last_human_activity` is the most
/// recently recorded human activity (for idle expiry; `None` = no human
/// seen yet). Returns only the decisions for due tasks, in input order
/// (deterministic).
#[must_use]
pub fn due_tasks<F>(
    tasks: &[ScheduledTask],
    mut last_fired: F,
    last_human_activity: Option<Timestamp>,
    now: Timestamp,
) -> Vec<DueDecision>
where
    F: FnMut(ScheduledTaskId) -> Option<Timestamp>,
{
    tasks
        .iter()
        .map(|task| decide(task, last_fired(task.id), last_human_activity, now))
        .filter(|decision| decision.due)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_actions::SkillId;
    use serde_json::json;

    fn at(unix_secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(unix_secs).expect("valid unix seconds")
    }

    fn task_with(interval: Duration) -> ScheduledTask {
        ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(7)),
            SkillId::new(),
            json!({}),
            interval,
            "being",
        )
    }

    fn task_with_cron(cron: &str) -> ScheduledTask {
        task_with(Duration::seconds(120)).with_cron_expression(cron)
    }

    // (1) Pure due-checking logic with an injected clock.
    #[test]
    fn fires_at_interval_not_before_and_not_again_until_next_window() {
        let interval = Duration::seconds(60);
        let start = at(0);

        // Never fired -> due immediately at time 0.
        assert!(is_due(interval, None, start));

        // Fired at time 0 -> last_fired = 0.
        let last_fired = Some(at(0));

        // now = 30s: NOT yet due (30 < 0 + 60).
        assert!(!is_due(interval, last_fired, at(30)));

        // now = 60s: due (60 >= 0 + 60).
        assert!(is_due(interval, last_fired, at(60)));

        // Fired now at 60s -> last_fired = 60. now = 90s: NOT again
        // (90 < 60 + 60). now = 120s: due again (120 >= 60 + 60).
        let last_fired = Some(at(60));
        assert!(!is_due(interval, last_fired, at(90)));
        assert!(is_due(interval, last_fired, at(120)));
    }

    #[test]
    fn never_fired_is_due_immediately() {
        let task = task_with(Duration::seconds(60));
        let decision = decide(&task, None, None, at(0));
        assert!(decision.due);
        assert!(decision.key.is_some());
    }

    #[test]
    fn disabled_task_is_never_due() {
        // Family-agency (Phase 4): the kill switch overrides due-checking.
        // The same task that would otherwise fire immediately (never
        // fired) does NOT fire when enabled=false.
        let task = task_with(Duration::seconds(60)).with_enabled(false);
        let decision = decide(&task, None, None, at(0));
        assert!(!decision.due, "disabled task does not fire");
        assert!(decision.key.is_none(), "no key when not firing");

        // Re-enabling -> fires again.
        let reenabled = task.with_enabled(true);
        assert!(
            decide(&reenabled, None, None, at(0)).due,
            "re-enabling restores firing"
        );
    }

    #[test]
    fn not_due_has_no_key() {
        let task = task_with(Duration::seconds(60));
        let decision = decide(&task, Some(at(0)), None, at(30));
        assert!(!decision.due);
        assert!(decision.key.is_none());
    }

    // (2) Deterministic key: same logical firing -> same key across two
    //     separate evaluations (crash/restart dedup through the outbox).
    #[test]
    fn same_logical_firing_yields_same_key_across_evaluations() {
        let task = task_with(Duration::seconds(60));

        // Two different now values in the SAME interval window [60, 120):
        let eval_a = firing_key(task.id, task.interval, at(65));
        let eval_b = firing_key(task.id, task.interval, at(119));
        assert_eq!(eval_a, eval_b, "same window -> same key (restart dedup)");

        // Next window [120, 180) -> different key (no incorrect dedup).
        let next_window = firing_key(task.id, task.interval, at(120));
        assert_ne!(eval_a, next_window);
    }

    #[test]
    fn key_is_independent_of_process_memory() {
        // The key depends only on task_id + interval + now, not on any tick state.
        let task = task_with(Duration::seconds(30));
        let key1 = firing_key(task.id, task.interval, at(45));
        // "Restart": a new evaluation of the same window with a different now.
        let key2 = firing_key(task.id, task.interval, at(59));
        assert_eq!(key1, key2);
        assert!(key1.starts_with("schedule-"));
    }

    #[test]
    fn nonpositive_interval_is_always_due_with_stable_key() {
        assert!(is_due(Duration::zero(), Some(at(100)), at(100)));
        let task = task_with(Duration::zero());
        // The key stays well-defined even with a degenerate interval.
        let _ = firing_key(task.id, task.interval, at(7));
    }

    #[test]
    fn due_tasks_returns_only_due_in_order() {
        let a = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(1)),
            SkillId::new(),
            json!({}),
            Duration::seconds(60),
            "b",
        );
        let b = ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(2)),
            SkillId::new(),
            json!({}),
            Duration::seconds(60),
            "b",
        );
        let tasks = vec![a.clone(), b.clone()];
        // a fired recently (not due), b never fired (due).
        let due = due_tasks(
            &tasks,
            |id| {
                if id == a.id {
                    Some(at(50))
                } else {
                    None
                }
            },
            None,
            at(60),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, b.id);
    }

    // (Phase 4) expire-if-no-human: idle_expired + decide integration.
    #[test]
    fn idle_expired_logic() {
        let idle = Some(Duration::seconds(100));
        // No idle cap -> never expires.
        assert!(!idle_expired(None, None, at(1_000_000)));
        // Idle cap + no human ever -> expired immediately.
        assert!(idle_expired(idle, None, at(0)));
        // Human active recently (50s ago) -> not expired (50 < 100).
        assert!(!idle_expired(idle, Some(at(0)), at(50)));
        // Human active long ago (150s) -> expired (150 > 100).
        assert!(idle_expired(idle, Some(at(0)), at(150)));
        // Non-positive cap -> never expires (safe degenerate case).
        assert!(!idle_expired(Some(Duration::zero()), None, at(1_000_000)));
    }

    #[test]
    fn decide_respects_idle_expiry() {
        // A task that would otherwise fire immediately, but idle cap + no
        // human -> doesn't fire; fires again once human activity resumes.
        let task = task_with(Duration::seconds(60)).with_expire_after_idle(Duration::seconds(100));
        // No human -> expired -> doesn't fire.
        assert!(!decide(&task, None, None, at(0)).due);
        // Human active now -> fires (not yet idle).
        assert!(decide(&task, None, Some(at(0)), at(0)).due);
        // Human 200s ago -> idle exceeded -> doesn't fire.
        assert!(!decide(&task, None, Some(at(0)), at(200)).due);
    }

    #[test]
    fn parse_cron_accepts_standard_expression() {
        assert!(parse_cron("0 * * * *").is_some());
        assert!(parse_cron("not a cron").is_none());
    }

    #[test]
    fn cron_fires_on_schedule_not_before_occurrence() {
        // Every hour at minute 0 (UTC).
        let cron = "0 * * * *";
        let hour_start = at(3_600); // 01:00:00

        // Never fired -> due at the first occurrence.
        assert!(is_due_cron(cron, None, hour_start));

        // Fired at 01:00 -> not again at 01:30 (same hour window).
        let last = Some(hour_start);
        assert!(!is_due_cron(cron, last, at(3_600 + 1_800)));

        // Next hour start 02:00 -> due again.
        assert!(is_due_cron(cron, last, at(7_200)));
    }

    #[test]
    fn cron_firing_key_is_stable_within_occurrence() {
        let task = task_with_cron("0 * * * *");
        let key_a = cron_firing_key(task.id, "0 * * * *", at(3_650));
        let key_b = cron_firing_key(task.id, "0 * * * *", at(3_699));
        assert_eq!(key_a, key_b, "same hour occurrence -> same key");
        assert!(key_a.starts_with("schedule-"));

        let next_hour = cron_firing_key(task.id, "0 * * * *", at(7_200));
        assert_ne!(key_a, next_hour, "different occurrence -> different key");
    }

    #[test]
    fn decide_uses_cron_when_expression_set() {
        let task = task_with_cron("* * * * *");
        let decision = decide(&task, None, None, at(60));
        assert!(decision.due);
        let key = decision.key.expect("cron due has key");
        assert_eq!(key, cron_firing_key(task.id, "* * * * *", at(60)));

        // A 120s interval would block it (90 < 0+120), but the minute cron fires (last occurrence 60s).
        let interval_task = task_with(Duration::seconds(120));
        assert!(!decide(&interval_task, Some(at(0)), None, at(90)).due);
        assert!(decide(&task, Some(at(0)), None, at(90)).due);
    }

    #[test]
    fn invalid_cron_expression_is_never_due() {
        let task = task_with(Duration::seconds(60)).with_cron_expression("not valid");
        assert!(!decide(&task, None, None, at(0)).due);
        assert!(decide(&task, None, None, at(0)).key.is_none());
    }
}

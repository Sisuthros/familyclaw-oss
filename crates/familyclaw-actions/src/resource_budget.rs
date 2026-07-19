//! Resource budget and lease: the backpressure decision layer (Layer A, generic).
//!
//! This module decides **when** a task must be suspended due to backpressure
//! (see [`crate::task::TaskStatus::Suspended`]). It does NOT run tasks
//! itself and does not read the clock — it only tracks how many concurrent
//! executions are in progress and grants [`ResourceLease`] leases for as
//! long as the budget allows.
//!
//! ## Model
//! - **Global concurrency cap** (`max_concurrent`): the upper bound on all
//!   tasks running at once.
//! - **Per-skill concurrency cap** (`per_skill_concurrency`): the upper
//!   bound on concurrent executions of a single skill ([`SkillId`]).
//! - **Queue length cap** (`max_queue_len`): the caller reports the current
//!   queue length; the budget rejects a new acquisition if the queue is
//!   already full (fail-closed, no unbounded queue).
//!
//! ## Fail-closed
//! If any cap is reached, [`ResourceBudget::try_acquire`] returns
//! [`AcquireOutcome::Unavailable`] with a **generic, secret-free reason** —
//! this reason fits directly into [`crate::task::TaskQueue::suspend`]'s
//! `reason` field. No panicking, no busy-loop: the caller suspends the task
//! and retries later once a [`ResourceLease`] is released.
//!
//! ## RAII
//! [`ResourceLease`] releases its reserved capacity automatically when it is
//! dropped ([`Drop`]). This way the counter cannot leak even if execution panics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ids::SkillId;

/// The backpressure budget's caps. All caps are optional: `None` = that
/// limit is not enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    /// Global upper bound on concurrent executions (`None` = unbounded).
    pub max_concurrent: Option<usize>,
    /// Per-skill upper bound on concurrent executions (`None` = unbounded).
    pub per_skill_concurrency: Option<usize>,
    /// The largest allowed queue length when making a new acquisition (`None` = unbounded).
    pub max_queue_len: Option<usize>,
}

impl BudgetLimits {
    /// An unbounded budget — enforces no cap (default / testing).
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_concurrent: None,
            per_skill_concurrency: None,
            max_queue_len: None,
        }
    }
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self::unbounded()
    }
}

/// The outcome of [`ResourceBudget::try_acquire`]: either a granted lease or
/// a generic reason for suspension.
#[derive(Debug)]
pub enum AcquireOutcome {
    /// The budget allowed it: a lease was granted. Capacity is released when
    /// the [`ResourceLease`] is dropped.
    Granted(ResourceLease),
    /// The budget did not allow it. Contains a secret-free reason that fits
    /// directly as [`crate::task::TaskQueue::suspend`]'s `reason` argument.
    Unavailable(String),
}

/// Shared, thread-safe counter of concurrent executions, per skill.
#[derive(Debug, Default)]
struct BudgetState {
    /// Total number of executions in progress.
    total_active: usize,
    /// Skill → its number of executions in progress.
    per_skill_active: HashMap<SkillId, usize>,
}

/// The backpressure budget. A cloneable handle to shared state (internal
/// `Arc<Mutex>`), so multiple workers can share the same budget.
#[derive(Debug, Clone)]
pub struct ResourceBudget {
    limits: BudgetLimits,
    state: Arc<Mutex<BudgetState>>,
}

impl ResourceBudget {
    /// Builds a budget with the given caps.
    #[must_use]
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            state: Arc::new(Mutex::new(BudgetState::default())),
        }
    }

    /// Attempts to reserve capacity for one skill execution.
    ///
    /// `current_queue_len` is the caller-reported current queue length — the
    /// budget rejects the acquisition if it has already reached
    /// `max_queue_len` (fail-closed, prevents unbounded queue growth).
    ///
    /// Returns [`AcquireOutcome::Granted`] with a lease if all caps allow
    /// it, otherwise [`AcquireOutcome::Unavailable`] with a generic reason.
    /// Nothing is recorded when the acquisition is rejected (fail-closed).
    #[must_use]
    pub fn try_acquire(&self, skill_id: SkillId, current_queue_len: usize) -> AcquireOutcome {
        // The queue-length cap is checked before taking the lock — it does not affect state.
        if let Some(max_q) = self.limits.max_queue_len {
            if current_queue_len >= max_q {
                return AcquireOutcome::Unavailable(format!(
                    "queue_length budget exhausted ({current_queue_len}/{max_q})"
                ));
            }
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(max_c) = self.limits.max_concurrent {
            if state.total_active >= max_c {
                return AcquireOutcome::Unavailable(format!(
                    "max_concurrent budget exhausted ({}/{max_c})",
                    state.total_active
                ));
            }
        }

        if let Some(max_s) = self.limits.per_skill_concurrency {
            let active = state.per_skill_active.get(&skill_id).copied().unwrap_or(0);
            if active >= max_s {
                return AcquireOutcome::Unavailable(format!(
                    "per_skill_concurrency budget exhausted ({active}/{max_s})"
                ));
            }
        }

        // All caps allow it: record the acquisition.
        state.total_active += 1;
        *state.per_skill_active.entry(skill_id).or_insert(0) += 1;

        AcquireOutcome::Granted(ResourceLease {
            skill_id,
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    /// Total number of executions in progress (diagnostics/testing).
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_active
    }

    /// Number of executions in progress for a single skill (diagnostics/testing).
    #[must_use]
    pub fn active_for_skill(&self, skill_id: SkillId) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .per_skill_active
            .get(&skill_id)
            .copied()
            .unwrap_or(0)
    }
}

/// Capacity reserved by a single execution. Releases the reservation
/// automatically when dropped ([`Drop`]) — even during a panic, so the
/// counter cannot leak.
#[derive(Debug)]
pub struct ResourceLease {
    skill_id: SkillId,
    state: Arc<Mutex<BudgetState>>,
    released: bool,
}

impl ResourceLease {
    /// Releases the lease explicitly (idempotent: a repeated call / a
    /// subsequent [`Drop`] does not decrement the counter again).
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.total_active = state.total_active.saturating_sub(1);
        if let Some(active) = state.per_skill_active.get_mut(&self.skill_id) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                state.per_skill_active.remove(&self.skill_id);
            }
        }
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(
        max_c: Option<usize>,
        per_skill: Option<usize>,
        max_q: Option<usize>,
    ) -> BudgetLimits {
        BudgetLimits {
            max_concurrent: max_c,
            per_skill_concurrency: per_skill,
            max_queue_len: max_q,
        }
    }

    #[test]
    fn unbounded_budget_always_grants() {
        let budget = ResourceBudget::new(BudgetLimits::unbounded());
        let skill = SkillId::new();
        for _ in 0..1000 {
            assert!(matches!(
                budget.try_acquire(skill, 0),
                AcquireOutcome::Granted(_)
            ));
        }
    }

    #[test]
    fn max_concurrent_suspends_when_exhausted() {
        let budget = ResourceBudget::new(limits(Some(2), None, None));
        let skill = SkillId::new();
        let _l1 = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        let _l2 = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        // The third exceeds the global cap → suspension with a generic reason.
        match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(_) => panic!("should have been unavailable"),
            AcquireOutcome::Unavailable(reason) => {
                assert!(reason.contains("max_concurrent"), "reason: {reason}");
            }
        }
        assert_eq!(budget.active_count(), 2);
    }

    #[test]
    fn lease_release_frees_capacity_for_resume() {
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let lease = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        assert!(matches!(
            budget.try_acquire(skill, 0),
            AcquireOutcome::Unavailable(_)
        ));
        // Release the lease → capacity is restored → a new acquisition succeeds (resume).
        drop(lease);
        assert_eq!(budget.active_count(), 0);
        assert!(matches!(
            budget.try_acquire(skill, 0),
            AcquireOutcome::Granted(_)
        ));
    }

    #[test]
    fn per_skill_concurrency_is_independent_per_skill() {
        let budget = ResourceBudget::new(limits(None, Some(1), None));
        let skill_a = SkillId::new();
        let skill_b = SkillId::new();
        let _a = match budget.try_acquire(skill_a, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        // skill_a is full...
        assert!(matches!(
            budget.try_acquire(skill_a, 0),
            AcquireOutcome::Unavailable(_)
        ));
        // ...but skill_b has its own budget.
        assert!(matches!(
            budget.try_acquire(skill_b, 0),
            AcquireOutcome::Granted(_)
        ));
    }

    #[test]
    fn queue_length_limit_fails_closed_without_recording() {
        let budget = ResourceBudget::new(limits(None, None, Some(10)));
        let skill = SkillId::new();
        // The queue is already full → rejected WITHOUT recording (active stays 0).
        match budget.try_acquire(skill, 10) {
            AcquireOutcome::Granted(_) => panic!("should reject full queue"),
            AcquireOutcome::Unavailable(reason) => {
                assert!(reason.contains("queue_length"), "reason: {reason}");
            }
        }
        assert_eq!(
            budget.active_count(),
            0,
            "a rejected acquisition must not be recorded"
        );
    }

    #[test]
    fn unavailable_reason_has_no_secret() {
        // The reason is derived only from caps + counters, not from a
        // payload — so it cannot contain secrets. This test locks in the invariant.
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let _l = budget.try_acquire(skill, 0);
        if let AcquireOutcome::Unavailable(reason) = budget.try_acquire(skill, 0) {
            assert!(!reason.contains("sk-"), "reason: {reason}");
            assert!(!reason.to_lowercase().contains("token"), "reason: {reason}");
            assert!(
                !reason.to_lowercase().contains("bearer"),
                "reason: {reason}"
            );
        } else {
            panic!("expected unavailable");
        }
    }

    #[test]
    fn explicit_release_is_idempotent() {
        let budget = ResourceBudget::new(limits(Some(1), None, None));
        let skill = SkillId::new();
        let mut lease = match budget.try_acquire(skill, 0) {
            AcquireOutcome::Granted(l) => l,
            AcquireOutcome::Unavailable(r) => panic!("unexpected: {r}"),
        };
        lease.release();
        lease.release(); // repeated — must not underflow the counter
        assert_eq!(budget.active_count(), 0);
        drop(lease); // drop after a release call — must not decrement again
        assert_eq!(budget.active_count(), 0);
    }
}

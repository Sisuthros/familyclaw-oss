//! [`SchedulerRunner`]: a thin asynchronous tick loop — the **only** part
//! that touches real time.
//!
//! The runner wakes up at a fixed interval ([`tokio::time::interval`]),
//! calls the scheduler's pure due-checking logic with the **real current
//! time**, and dispatches due tasks idempotently ([`Scheduler::tick`]). The
//! decision logic stays pure and testable without real time — the runner
//! just feeds it the clock.
//!
//! ## Cancellation (kill switch)
//! The loop stops cleanly when [`CancellationSignal`] is triggered (a
//! `cancel()` call **or** dropping it). The implementation uses a
//! [`tokio::sync::watch`] channel: dropping the sender closes the channel
//! and the loop observes it -> stops. This way both an explicit shutdown
//! signal and dropping the handle stop the scheduler.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use familyclaw_actions::ActionRuntime;
use familyclaw_core::time::Timestamp;
use tokio::sync::{watch, Mutex};

use crate::dispatch::Scheduler;

/// Shared handle to the scheduler while it's running (family-agency
/// operator surface).
///
/// [`SchedulerRunner::run_shared`] returns this so that, alongside the tick
/// loop, e.g. a gateway can toggle tasks on/off
/// ([`Scheduler::set_task_enabled`]). The lock is held **briefly** both in
/// the tick (per due-check) and in an operator mutation — never for long.
pub type SchedulerHandle = Arc<Mutex<Scheduler>>;

/// Cancellation signal for the scheduler loop (kill switch).
///
/// Keep this handle outside the scheduler. [`CancellationSignal::cancel`]
/// (or dropping the handle) stops the loop cleanly on the next tick, or
/// immediately if it's waiting.
#[derive(Debug)]
pub struct CancellationSignal {
    tx: watch::Sender<bool>,
}

impl CancellationSignal {
    /// Requests that the loop stop.
    ///
    /// Idempotent: calling it more than once is safe. The effect is the
    /// same as dropping the handle.
    pub fn cancel(&self) {
        // A send error means the receiver has already been dropped (the
        // loop has already stopped) — in that case there's nothing to stop.
        let _ = self.tx.send(true);
    }
}

/// Internal cancellation receiver that the loop polls.
#[derive(Debug, Clone)]
struct CancellationToken {
    rx: watch::Receiver<bool>,
}

impl CancellationToken {
    /// Whether cancellation has been requested (either `cancel()` or the
    /// sender was dropped).
    fn is_cancelled(&self) -> bool {
        // A closed channel (sender dropped) => cancelled. Otherwise read the flag.
        if self.rx.has_changed().is_err() {
            return true;
        }
        *self.rx.borrow()
    }

    /// Waits until cancellation is triggered (flag set or channel closed).
    async fn cancelled(&mut self) {
        loop {
            if *self.rx.borrow() {
                return;
            }
            // `changed()` returns Err once the sender is dropped -> cancelled.
            if self.rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Creates a cancellation signal-token pair.
#[must_use]
fn cancellation_pair() -> (CancellationSignal, CancellationToken) {
    let (tx, rx) = watch::channel(false);
    (CancellationSignal { tx }, CancellationToken { rx })
}

/// An asynchronous, cancellable scheduler loop.
///
/// The runner owns the [`Scheduler`] and [`ActionRuntime`] for the duration
/// of the run and ticks them at a fixed interval. Start the run with
/// [`SchedulerRunner::run`]; it returns only once the loop is cancelled.
#[derive(Debug)]
pub struct SchedulerRunner {
    scheduler: Scheduler,
    runtime: ActionRuntime,
    period: StdDuration,
}

impl SchedulerRunner {
    /// Creates a runner with the given scheduler, action runtime, and tick
    /// period.
    ///
    /// `period` is the **runner's** wake-up interval (how often due-checking
    /// is evaluated) — distinct from any individual task's interval. Keep it
    /// smaller than or equal to the shortest task interval so due tasks are
    /// noticed in time.
    #[must_use]
    pub fn new(scheduler: Scheduler, runtime: ActionRuntime, period: StdDuration) -> Self {
        Self {
            scheduler,
            runtime,
            period,
        }
    }

    /// Runs the tick loop until `cancel` is triggered.
    ///
    /// Returns the cancellation signal (kill switch) used to stop the loop.
    /// `now_fn` injects the current time **inside the tick** — in
    /// production, [`familyclaw_core::time::now`]; in tests, a controllable
    /// clock. The loop itself only touches real time via
    /// [`tokio::time::interval`]; whatever *clock* is handed to tasks comes
    /// from `now_fn`, so the due-checking logic stays testable.
    ///
    /// Dispatch errors ([`Scheduler::tick`]) are logged and the loop
    /// **continues** — a transient error in one task doesn't bring down the
    /// whole scheduler.
    ///
    /// Requires being called from within a Tokio runtime (for
    /// [`tokio::spawn`]).
    pub fn run<F>(self, now_fn: F) -> CancellationSignal
    where
        F: Fn() -> Timestamp + Send + 'static,
    {
        let (signal, token) = cancellation_pair();
        let mut scheduler = self.scheduler;
        let mut runtime = self.runtime;
        let period = self.period;
        let mut token_loop = token;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            loop {
                tokio::select! {
                    biased;
                    () = token_loop.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        if token_loop.is_cancelled() {
                            break;
                        }
                        let now = now_fn();
                        if let Err(error) = scheduler.tick(&mut runtime, now).await {
                            tracing::warn!(%error, "scheduler tick failed — continuing");
                        }
                    }
                }
            }
        });

        signal
    }

    /// Like [`run`](Self::run), but also returns a **shared handle** to the
    /// scheduler ([`SchedulerHandle`]) for the operator surface
    /// (family-agency, Phase 4).
    ///
    /// The scheduler is placed behind an `Arc<Mutex<Scheduler>>`, and the
    /// returned handle lets e.g. a gateway toggle tasks on/off
    /// ([`Scheduler::set_task_enabled`]) through the same lock.
    ///
    /// ## The lock is NOT held across dispatch (`await`)
    /// Each tick does three steps: **(1)** takes the lock **only briefly**
    /// and collects due dispatch instructions ([`Scheduler::collect_due`],
    /// pure, no `await`), **(2)** releases the lock and runs idempotent
    /// dispatches ([`ActionRuntime::submit_task_idempotent`]) **without the
    /// lock**, **(3)** briefly reacquires the lock to record `last_fired`
    /// for successful firings ([`Scheduler::record_fired`]). This way, long
    /// dispatch I/O **doesn't** block operator-surface mutations
    /// (pause/resume/kill switch) — they fit in during step 2, while the
    /// lock is free. Previously the lock was held across the entire
    /// `tick().await`, which queued gateway commands behind a slow tick.
    ///
    /// The due decision stays correct: the key
    /// ([`crate::decision::firing_key`]) is stable within an interval
    /// window and dispatch is idempotent, so even if a task were disabled
    /// during dispatch, a firing already in progress completes at most
    /// once, and the `last_fired` record doesn't corrupt the next window.
    ///
    /// Requires a Tokio runtime ([`tokio::spawn`]).
    pub fn run_shared<F>(self, now_fn: F) -> (CancellationSignal, SchedulerHandle)
    where
        F: Fn() -> Timestamp + Send + 'static,
    {
        let (signal, token) = cancellation_pair();
        let handle: SchedulerHandle = Arc::new(Mutex::new(self.scheduler));
        let loop_handle = Arc::clone(&handle);
        let mut runtime = self.runtime;
        let period = self.period;
        let mut token_loop = token;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(period);
            loop {
                tokio::select! {
                    biased;
                    () = token_loop.cancelled() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        if token_loop.is_cancelled() {
                            break;
                        }
                        let now = now_fn();

                        // (1) Lock held only for the decision: collect due
                        //     dispatch instructions (pure, no await) and release the lock.
                        let due = {
                            let sched = loop_handle.lock().await;
                            sched.collect_due(now)
                        };

                        // (2) Dispatch WITHOUT the lock -> operator mutations
                        //     fit in even during a long dispatch.
                        for dispatch in due {
                            let result = runtime
                                .submit_task_idempotent(
                                    &dispatch.key,
                                    &dispatch.being_id,
                                    dispatch.skill_id,
                                    dispatch.payload,
                                    now,
                                )
                                .await;
                            match result {
                                Ok(_) => {
                                    // (3) Lock reacquired briefly, only to record the result.
                                    loop_handle.lock().await.record_fired(dispatch.task_id, now);
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "scheduler dispatch failed — continuing");
                                }
                            }
                        }
                    }
                }
            }
        });

        (signal, handle)
    }
}

/// Convenience function: runs the runner and returns the cancellation signal.
///
/// Equivalent to [`SchedulerRunner::run`] with the default clock
/// ([`familyclaw_core::time::now`]). Use [`SchedulerRunner::run`] directly
/// if you want to inject a clock in tests. Requires a Tokio runtime.
#[must_use]
pub fn run_until_cancelled(runner: SchedulerRunner) -> CancellationSignal {
    runner.run(familyclaw_core::time::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn now_at_secs(secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(secs).expect("valid unix seconds")
    }

    // (4) The runner is cancellable: start, cancel, verify it stops.
    #[tokio::test(start_paused = true)]
    async fn runner_stops_after_explicit_cancel() {
        // Count how many times now_fn is called (= number of ticks).
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let runner = SchedulerRunner::new(
            Scheduler::new(),
            ActionRuntime::new(),
            StdDuration::from_millis(10),
        );
        let signal = runner.run(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            now_at_secs(0)
        });

        // Let a few ticks happen in paused time.
        tokio::time::advance(StdDuration::from_millis(35)).await;
        tokio::task::yield_now().await;
        let before = calls.load(Ordering::SeqCst);
        assert!(before >= 1, "the loop should have ticked at least once");

        // Cancel and verify the loop stops (no more ticks).
        signal.cancel();
        tokio::task::yield_now().await;
        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let after = calls.load(Ordering::SeqCst);

        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let final_count = calls.load(Ordering::SeqCst);
        assert_eq!(after, final_count, "must not tick again after cancellation");
    }

    #[tokio::test(start_paused = true)]
    async fn runner_stops_when_signal_dropped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let runner = SchedulerRunner::new(
            Scheduler::new(),
            ActionRuntime::new(),
            StdDuration::from_millis(10),
        );
        let signal = runner.run(move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            now_at_secs(0)
        });

        tokio::time::advance(StdDuration::from_millis(25)).await;
        tokio::task::yield_now().await;

        // Drop the handle -> channel closes -> loop stops.
        drop(signal);
        tokio::task::yield_now().await;
        tokio::time::advance(StdDuration::from_millis(50)).await;
        tokio::task::yield_now().await;
        let after = calls.load(Ordering::SeqCst);

        tokio::time::advance(StdDuration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            after,
            calls.load(Ordering::SeqCst),
            "no more ticks after dropping"
        );
    }

    #[test]
    fn cancel_is_idempotent() {
        let (signal, token) = cancellation_pair();
        signal.cancel();
        signal.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn dropped_signal_marks_token_cancelled() {
        let (signal, token) = cancellation_pair();
        assert!(!token.is_cancelled());
        drop(signal);
        assert!(token.is_cancelled());
    }

    // ── Lock-not-held-across-await tests (PR: control plane doesn't queue
    //    behind a tick) ────────────────────────────────────────────────────
    //
    // These tests use REAL time (no start_paused) and a multi-thread
    // runtime, because they measure genuine concurrency between the
    // runner's tick loop and an external operator mutation through the
    // shared lock.

    use std::time::Duration as RealDuration;

    use async_trait::async_trait;
    use chrono::Duration as ChronoDuration;
    use familyclaw_actions::executor::{ActionExecutor, ActionRequest, ActionResult};
    use familyclaw_actions::manifest::SkillManifest;
    use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
    use familyclaw_actions::skills::Skill;
    use familyclaw_actions::SkillId;
    use tokio::sync::Notify;

    use crate::task::{ScheduledTask, ScheduledTaskId};

    /// A test skill whose execution **blocks** on a controlled release barrier.
    ///
    /// `execute` first signals that execution has started (`started`),
    /// counts the number of runs (`run_count`), and then waits on the
    /// `release` barrier before returning. This lets a test keep a single
    /// tick's dispatch "in progress" and prove that an operator mutation
    /// fits in while the lock is free.
    #[derive(Debug)]
    struct BarrierSkill {
        id: SkillId,
        started: Arc<Notify>,
        release: Arc<Notify>,
        run_count: Arc<AtomicUsize>,
    }

    impl BarrierSkill {
        fn new(id: SkillId) -> (Self, Arc<Notify>, Arc<Notify>, Arc<AtomicUsize>) {
            let started = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let run_count = Arc::new(AtomicUsize::new(0));
            let skill = Self {
                id,
                started: Arc::clone(&started),
                release: Arc::clone(&release),
                run_count: Arc::clone(&run_count),
            };
            (skill, started, release, run_count)
        }
    }

    #[async_trait]
    impl ActionExecutor for BarrierSkill {
        async fn execute(
            &self,
            request: ActionRequest,
        ) -> familyclaw_actions::Result<ActionResult> {
            self.run_count.fetch_add(1, Ordering::SeqCst);
            // Signal that execution has started, then wait for release.
            self.started.notify_one();
            self.release.notified().await;
            Ok(ActionResult::success(
                "barrier skill released",
                serde_json::Value::Null,
                request.now,
            ))
        }
    }

    impl Skill for BarrierSkill {
        fn manifest(&self) -> SkillManifest {
            SkillManifest {
                id: self.id,
                name: "barrier_test_skill".to_string(),
                version: "1.0.0".to_string(),
                description: "Test skill that blocks on a controllable barrier.".to_string(),
                permissions: vec![SkillPermission::ReadFiles],
                risk: ActionRisk::ReadOnly,
                approval_policy: ApprovalPolicy::AutoIfReadOnly,
                input_hint: None,
                output_hint: None,
                input_schema: serde_json::json!({ "type": "object" }),
                publisher: None,
                signature: None,
            }
        }
    }

    fn barrier_task(id: ScheduledTaskId, skill_id: SkillId) -> ScheduledTask {
        ScheduledTask::with_id(
            id,
            skill_id,
            serde_json::json!({}),
            ChronoDuration::seconds(60),
            "being",
        )
    }

    // (core) An operator mutation (set_task_enabled) completes WHILE a
    // long tick dispatch is in progress — proves the lock isn't held
    // across the await.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_mutation_completes_while_long_action_in_progress() {
        let skill_id = SkillId::new();
        let (skill, started, release, _run_count) = BarrierSkill::new(skill_id);

        let mut runtime = ActionRuntime::new();
        runtime
            .register_skill(skill)
            .expect("register barrier skill");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(101));
        let other_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(102));
        sched.register(barrier_task(task_id, skill_id));
        sched.register(barrier_task(other_id, skill_id));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(5));
        let (signal, handle) = runner.run_shared(|| now_at_secs(0));

        // Wait for the first due task's dispatch to be IN PROGRESS
        // (the skill is blocked on the barrier). At this point the
        // dispatch loop is awaiting and does NOT hold the scheduler lock
        // (it was collected and released before the await).
        tokio::time::timeout(RealDuration::from_secs(5), started.notified())
            .await
            .expect("barrier skill should have started");

        // Operator mutation: must complete IMMEDIATELY even while dispatch
        // is in progress — the lock is free during the await.
        let mutation = tokio::time::timeout(RealDuration::from_secs(2), async {
            let mut s = handle.lock().await;
            s.set_task_enabled(other_id, false)
        })
        .await;
        assert!(
            mutation.is_ok(),
            "set_task_enabled got stuck behind dispatch — the lock was held across the await"
        );
        assert!(
            mutation.unwrap(),
            "known id -> set_task_enabled returns true"
        );

        // Release the barrier so the runtime doesn't hang, and stop the loop.
        release.notify_waiters();
        signal.cancel();
    }

    // (dispatch-once) A due task is dispatched exactly once per window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_dispatches_due_task_exactly_once() {
        let skill_id = SkillId::new();
        let (skill, started, release, run_count) = BarrierSkill::new(skill_id);
        // Release the barrier immediately for every waiter, so dispatch
        // returns right away (doesn't hang) — this test measures firing counts.
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(111));
        sched.register(barrier_task(task_id, skill_id));

        // Fixed now -> same interval window on every tick; the idempotency
        // key is the same, so multiple ticks in the same window must NOT refire.
        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(1000));

        // Wait for the first firing.
        tokio::time::timeout(RealDuration::from_secs(5), started.notified())
            .await
            .expect("task should fire once");
        // Let many ticks pass within the same window.
        tokio::time::sleep(RealDuration::from_millis(60)).await;
        signal.cancel();
        tokio::time::sleep(RealDuration::from_millis(20)).await;

        assert_eq!(
            run_count.load(Ordering::SeqCst),
            1,
            "same window -> outbox dedup -> exactly one firing"
        );
    }

    // (disabled stays quiet) A disabled task dispatches nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_disabled_task_stays_quiet() {
        let skill_id = SkillId::new();
        let (skill, _started, release, run_count) = BarrierSkill::new(skill_id);
        release.notify_waiters(); // no waiters yet; just in case

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let task_id = ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(121));
        sched.register(barrier_task(task_id, skill_id).with_enabled(false));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(0));

        // Let several ticks pass — a disabled task must not fire.
        tokio::time::sleep(RealDuration::from_millis(60)).await;
        signal.cancel();
        tokio::time::sleep(RealDuration::from_millis(10)).await;

        assert_eq!(
            run_count.load(Ordering::SeqCst),
            0,
            "a disabled task does not fire in the shared runner"
        );
    }

    // (cancellation) The shared runner stops on the cancel signal even
    // when dispatch doesn't hold the lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_runner_stops_on_cancel() {
        let skill_id = SkillId::new();
        let (skill, _started, release, run_count) = BarrierSkill::new(skill_id);
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        sched.register(barrier_task(
            ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(131)),
            skill_id,
        ));

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(3));
        let (signal, _handle) = runner.run_shared(|| now_at_secs(0));

        tokio::time::sleep(RealDuration::from_millis(30)).await;
        signal.cancel();
        // Let the loop observe the cancellation and stop.
        tokio::time::sleep(RealDuration::from_millis(20)).await;
        let after_cancel = run_count.load(Ordering::SeqCst);
        tokio::time::sleep(RealDuration::from_millis(40)).await;
        assert_eq!(
            run_count.load(Ordering::SeqCst),
            after_cancel,
            "no new firings after cancellation"
        );
    }

    // (no deadlock) Concurrent set_task_enabled calls alongside ticking
    // don't deadlock: the loop keeps dispatching while the lock is mostly free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_runner_no_deadlock_under_concurrent_mutations() {
        let skill_id = SkillId::new();
        let (skill, _started, release, _run_count) = BarrierSkill::new(skill_id);
        let release_for_task = Arc::clone(&release);
        tokio::spawn(async move {
            loop {
                release_for_task.notify_waiters();
                tokio::time::sleep(RealDuration::from_millis(1)).await;
            }
        });

        let mut runtime = ActionRuntime::new();
        runtime.register_skill(skill).expect("register");

        let mut sched = Scheduler::new();
        let ids: Vec<ScheduledTaskId> = (0..5)
            .map(|n| ScheduledTaskId::from_uuid(uuid::Uuid::from_u128(200 + n)))
            .collect();
        for id in &ids {
            sched.register(barrier_task(*id, skill_id));
        }

        let runner = SchedulerRunner::new(sched, runtime, RealDuration::from_millis(2));
        let (signal, handle) = runner.run_shared(|| now_at_secs(0));

        // Hammer operator mutations concurrently with ticking.
        let hammer = {
            let handle = Arc::clone(&handle);
            let ids = ids.clone();
            tokio::spawn(async move {
                for round in 0..200u32 {
                    let mut s = handle.lock().await;
                    for id in &ids {
                        s.set_task_enabled(*id, round % 2 == 0);
                    }
                    drop(s);
                    tokio::task::yield_now().await;
                }
            })
        };

        // The whole thing must complete comfortably within the time limit (no deadlock).
        let done = tokio::time::timeout(RealDuration::from_secs(10), hammer).await;
        assert!(done.is_ok(), "concurrent mutations deadlocked");
        done.unwrap().expect("hammer task panicked");

        signal.cancel();
    }
}

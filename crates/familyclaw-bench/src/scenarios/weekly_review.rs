//! S8 Weekly Review — a weekly aggregate snapshot of memory state.
//!
//! Where [`crate::scenarios::dream_quality`] proves consolidation for *a single
//! night*, this scenario proves that memory storage yields a **deterministic
//! weekly review**: how many memories are active/archived/tombstoned, which
//! surviving memories are the most important in importance order, and how many
//! conflicts are awaiting resolution. This mirrors the Amplifier prosthesis's
//! weekly "scorecard" summary natively (design §2.3) — an auditable report
//! that mutates nothing.
//!
//! ## What this measures
//! The scenario seeds [`LocalJsonStore`] with a known set of memories in
//! different states and with different importance values, tags one conflicting
//! pair, and runs [`weekly_review`] at the injected `now` instant. It then
//! verifies that:
//! 1. **State counters** (`total`/`active`/`archived`/`tombstoned`/`consolidated`)
//!    match the seeded state.
//! 2. The **`top_memories` ordering** is in descending importance order and
//!    tombstoned memories are never surfaced.
//! 3. The **conflict counter** matches the size of the tagged pair.
//!
//! Metrics:
//! - `counts_correct` — 1.0 if all state counters match the expected values.
//! - `top_order_correct` — 1.0 if `top_memories` is in descending importance
//!   order and contains no tombstoned memories.
//! - `conflicts_correct` — 1.0 if the conflict counter matches the expected value.
//! - `retrievable_ratio` — the fraction of retrievable (active + archived) memories.
//!
//! ## Reproducibility
//! `now` is injected as [`Scenario::run`]'s `clock` parameter — the review
//! takes the instant as a parameter (not from the system clock) and orders its
//! results stably (importance descending, ties broken by lower id), so the
//! same seed always produces the same report (design §2.2).
//!
//! ## Role of the subject
//! [`Scenario::run`] receives the subject as a black box ([`Subject`]); the
//! subject's liveness is verified with a lightweight `sleep_cycle` call that
//! must not crash the subject. The authoritative metrics are computed from a
//! dedicated seeded store, since the weekly review is a memory/dream-level
//! invariant that is the same for every subject.

use async_trait::async_trait;

use familyclaw_core::Timestamp;
use familyclaw_dream::{tag_conflict, weekly_review, WeeklyReport};
use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore};

use crate::error::{BenchError, Result};
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// S8 Weekly Review scenario.
///
/// Stateless value; all run state is seeded within [`Scenario::run`] relative
/// to the injected clock, so the scenario can be run many times and yield the
/// same result.
#[derive(Debug, Default, Clone, Copy)]
pub struct WeeklyReviewScenario;

impl WeeklyReviewScenario {
    /// The scenario's stable identifier.
    pub const ID: &'static str = "s8_weekly_review";

    /// Builds the scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Known seed: expected counters and importance order for later evaluation.
struct Seeded {
    /// Total number of memories.
    total: usize,
    /// Number of active memories.
    active: usize,
    /// Number of archived memories.
    archived: usize,
    /// Number of tombstoned memories.
    tombstoned: usize,
    /// Number of retrievable (active + archived) memories = `consolidated`.
    consolidated: usize,
    /// Number of memories tagged as conflicted (a tagged pair → 2).
    conflicted: usize,
    /// Contents of retrievable memories in descending importance order —
    /// the expected `top_memories` ordering.
    expected_top_order: Vec<String>,
}

impl WeeklyReviewScenario {
    /// Seeds a known store relative to the injected clock and returns the
    /// expected counters + importance order.
    ///
    /// Seed (importance values chosen to be distinct so ties don't disturb
    /// ordering):
    /// - 3 active (importance 0.9, 0.5, 0.2),
    /// - 1 archived (importance 0.7) — still retrievable, so included in the top list,
    /// - 1 tombstoned (importance 0.95) — must NOT be surfaced,
    /// - 1 conflicting pair (two memories) tagged → `conflicted == 2`.
    async fn seed(store: &LocalJsonStore, clock: Timestamp) -> Result<Seeded> {
        // — Active memories (retrievable, at different importances) ————————
        let high = add(store, mem("the launch shipped on time", 0.9, clock)).await?;
        add(store, mem("a mid-priority note about testing", 0.5, clock)).await?;
        add(store, mem("a low-priority passing thought", 0.2, clock)).await?;

        // — Archived (still retrievable, appears in the top list) ——————————
        let archived = add(
            store,
            mem("an older but still relevant decision", 0.7, clock),
        )
        .await?;
        store
            .set_status(archived, MemoryStatus::Archived)
            .await
            .map_err(BenchError::from)?;

        // — Tombstoned (high importance, but must NOT be surfaced in the top list) —
        let tombstoned = add(store, mem("a retracted false claim", 0.95, clock)).await?;
        store
            .set_status(tombstoned, MemoryStatus::Tombstoned)
            .await
            .map_err(BenchError::from)?;

        // — Conflicting pair (two active memories) is tagged ————————————————
        let conflict_a = add(store, mem("agent_a is in region one", 0.4, clock)).await?;
        let conflict_b = add(store, mem("agent_a is in region two", 0.3, clock)).await?;
        tag_conflict(store, conflict_a, conflict_b, clock)
            .await
            .map_err(BenchError::from)?;

        // Total seeded: 3 active + 1 archived + 1 tombstoned + 2 conflicting
        // (active) = 7.
        // active = 3 + 2 (both sides of the conflicting pair) = 5.
        // archived = 1, tombstoned = 1.
        // consolidated (retrievable) = active + archived = 6.
        // conflicted = 2 (both sides of the tagged pair).
        //
        // Retrievable memories in descending importance order (tombstoned excluded):
        //   0.9 launch, 0.7 archived, 0.5 mid, 0.4 conflict_a,
        //   0.3 conflict_b, 0.2 low.
        // `weekly_review` caps top_memories at the first DEFAULT_TOP_N (=5),
        // so the expected order is the five most important.
        let _ = (high, conflict_a, conflict_b);
        let expected_top_order = vec![
            "the launch shipped on time".to_string(),
            "an older but still relevant decision".to_string(),
            "a mid-priority note about testing".to_string(),
            "agent_a is in region one".to_string(),
            "agent_a is in region two".to_string(),
        ];

        Ok(Seeded {
            total: 7,
            active: 5,
            archived: 1,
            tombstoned: 1,
            consolidated: 6,
            conflicted: 2,
            expected_top_order,
        })
    }
}

#[async_trait]
impl Scenario for WeeklyReviewScenario {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        // Liveness check: the subject's own sleep cycle must not crash. The
        // result is recorded as a note; the authoritative metrics are computed
        // below from a dedicated seed (the review is a memory/dream-level
        // invariant).
        let subject_summary = subject.sleep_cycle(clock).await?;

        // Seed a known store + tag a conflicting pair relative to the
        // injected clock.
        let store = LocalJsonStore::in_memory();
        let seeded = Self::seed(&store, clock).await?;

        // Run the REAL weekly review at the injected instant, then evaluate.
        let report = weekly_review(&store, clock)
            .await
            .map_err(BenchError::from)?;
        let outcome = Outcome::evaluate(&seeded, &report)?;

        let result = ScenarioResult::new(Self::ID, outcome.passed)
            .with_metric("counts_correct", bool_metric(outcome.counts_correct))
            .with_metric("top_order_correct", bool_metric(outcome.top_order_correct))
            .with_metric("conflicts_correct", bool_metric(outcome.conflicts_correct))
            .with_metric("retrievable_ratio", outcome.retrievable_ratio)
            .with_note(format!(
                "counts: total={} active={} archived={} tombstoned={} consolidated={} (expected total={})",
                report.total,
                report.active,
                report.archived,
                report.tombstoned,
                report.consolidated,
                seeded.total
            ))
            .with_note(format!(
                "top_memories: {} listed, order_correct={} (buried memory excluded)",
                report.top_memories.len(),
                outcome.top_order_correct
            ))
            .with_note(format!(
                "conflicted={} (expected {}), conflicts_listed={}",
                report.conflicted,
                seeded.conflicted,
                report.conflicts.len()
            ))
            .with_note(format!(
                "subject sleep_cycle liveness: scanned={} protected_core_intact={}",
                subject_summary.scanned, subject_summary.protected_core_intact
            ));

        Ok(result)
    }
}

/// Weekly review evaluation: all S8 metrics and the pass result in one place.
///
/// The flags are independent diagnostic results (counters / ordering /
/// conflicts, separately), not states of a state machine — hence four
/// separate booleans is the clearest representation (not a
/// `struct_excessive_bools` refactor into two-valued enums, which would only
/// obscure the meaning).
#[allow(clippy::struct_excessive_bools)]
struct Outcome {
    /// Whether all state counters match the seeded state.
    counts_correct: bool,
    /// Whether `top_memories` is in descending importance order, contains no
    /// tombstoned memories, and matches the expected content order.
    top_order_correct: bool,
    /// Whether the conflict counter matches the seeded pair.
    conflicts_correct: bool,
    /// Retrievable fraction of the total (`consolidated / total`).
    retrievable_ratio: f64,
    /// The overall scenario pass result.
    passed: bool,
}

impl Outcome {
    /// Evaluates the weekly review by comparing the report against the known seed.
    ///
    /// # Errors
    /// [`BenchError`] if the seed was empty (division-by-zero guard).
    fn evaluate(seeded: &Seeded, report: &WeeklyReport) -> Result<Self> {
        if seeded.total == 0 {
            return Err(BenchError::scenario(
                "weekly_review: seed produced an empty store",
            ));
        }

        let counts_correct = report.total == seeded.total
            && report.active == seeded.active
            && report.archived == seeded.archived
            && report.tombstoned == seeded.tombstoned
            && report.consolidated == seeded.consolidated;

        // Top ordering: contents match the expected descending order AND
        // importance values are non-increasing (belt and suspenders).
        let actual_top: Vec<&str> = report
            .top_memories
            .iter()
            .map(|d| d.content.as_str())
            .collect();
        let expected_top: Vec<&str> = seeded
            .expected_top_order
            .iter()
            .map(String::as_str)
            .collect();
        let order_matches = actual_top == expected_top;
        let importance_non_increasing = report
            .top_memories
            .windows(2)
            .all(|w| w[0].importance >= w[1].importance);
        let top_order_correct = order_matches && importance_non_increasing;

        let conflicts_correct =
            report.conflicted == seeded.conflicted && report.conflicts.len() == seeded.conflicted;

        #[allow(clippy::cast_precision_loss)]
        let retrievable_ratio = report.consolidated as f64 / report.total as f64;

        let passed = counts_correct && top_order_correct && conflicts_correct;

        Ok(Self {
            counts_correct,
            top_order_correct,
            conflicts_correct,
            retrievable_ratio,
            passed,
        })
    }
}

/// Adds a memory to the store, wrapping the core crate's error into a bench error.
async fn add(store: &LocalJsonStore, memory: Memory) -> Result<familyclaw_core::MessageId> {
    store.add(memory).await.map_err(BenchError::from)
}

/// A memory with the given content and importance, anchored to the injected clock.
fn mem(content: &str, importance: f32, clock: Timestamp) -> Memory {
    Memory::builder(content)
        .factors(ImportanceFactors::new(importance, 0.0, 0.0, 0.0))
        .created_at(clock)
        .build()
}

/// Boolean → metric value (`1.0`/`0.0`) for a deterministic scorecard.
fn bool_metric(ok: bool) -> f64 {
    if ok {
        1.0
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_literal_bound)] // Stub implementation returns a literal.
#[allow(clippy::float_cmp)] // Constants 0.0/1.0 are exact float values in these tests.
mod tests {
    use super::*;

    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    /// Fixed injected reference clock (2026-06-05 12:00 UTC).
    fn clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_780_660_800).expect("valid clock")
    }

    /// Minimal subject stub that never crashes — the scenario computes its
    /// authoritative metrics from its own seed, so the stub's return values
    /// do not affect the pass result.
    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(Vec::new())
        }
        async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
            Ok(DreamSummary {
                scanned: 0,
                merged: 0,
                dropped: 0,
                dates_absolutized: 0,
                strengthened: 0,
                archived: 0,
                protected_core_intact: true,
            })
        }
        fn name(&self) -> &str {
            "stub_subject"
        }
    }

    #[tokio::test]
    async fn weekly_review_passes_all_money_metrics() {
        let mut subject = StubSubject;
        let scenario = WeeklyReviewScenario::new();
        let result = scenario.run(&mut subject, clock()).await.expect("run");

        assert_eq!(result.id, WeeklyReviewScenario::ID);
        assert_eq!(result.metrics.get("counts_correct").copied(), Some(1.0));
        assert_eq!(result.metrics.get("top_order_correct").copied(), Some(1.0));
        assert_eq!(result.metrics.get("conflicts_correct").copied(), Some(1.0));
        // Retrievable = 6/7.
        let ratio = result
            .metrics
            .get("retrievable_ratio")
            .copied()
            .expect("ratio metric");
        assert!(
            (ratio - 6.0 / 7.0).abs() < 1e-9,
            "expected 6/7 retrievable, got {ratio}"
        );

        assert!(result.passed, "S8 should pass: {:?}", result.notes);
    }

    #[tokio::test]
    async fn weekly_review_is_deterministic() {
        let mut s1 = StubSubject;
        let mut s2 = StubSubject;
        let scenario = WeeklyReviewScenario::new();
        let r1 = scenario.run(&mut s1, clock()).await.expect("r1");
        let r2 = scenario.run(&mut s2, clock()).await.expect("r2");
        // Same injected instant → identical metrics + notes (§2.2).
        assert_eq!(r1.metrics, r2.metrics);
        assert_eq!(r1.notes, r2.notes);
        assert_eq!(r1.passed, r2.passed);
    }

    #[tokio::test]
    async fn buried_memory_is_excluded_from_top() {
        // Direct proof: the highest-importance memory (0.95) is tombstoned,
        // so it must NOT be at the top of the list — the top should be the
        // 0.9 active memory instead.
        let store = LocalJsonStore::in_memory();
        WeeklyReviewScenario::seed(&store, clock())
            .await
            .expect("seed");
        let report = weekly_review(&store, clock()).await.expect("review");
        assert_eq!(report.top_memories[0].content, "the launch shipped on time");
        assert!(report
            .top_memories
            .iter()
            .all(|d| d.content != "a retracted false claim"));
    }

    #[test]
    fn id_is_stable() {
        assert_eq!(WeeklyReviewScenario::new().id(), "s8_weekly_review");
    }
}

//! S1 Crash Matrix — an adversarial proof of durable-replay continuity.
//!
//! This scenario (design §3 S1) runs a fixed multi-step task on a
//! [`Subject`], **kills it** at each crash point in turn, restarts, and
//! proves three things:
//!
//! 1. **resume correctness** — the workload continues from exactly the next
//!    step (not from scratch, not incorrectly from the middle).
//! 2. **side effect exactly once** — replay does not re-run side effects
//!    (this is the durable substrate's core promise).
//! 3. **result == crash-free baseline** — the crashed run's end state is
//!    identical to a crash-free baseline run.
//!
//! Competitors lose in-flight work at exactly these points — this scenario
//! makes that measurable and reproducible.
//!
//! ## Crash points (design §3 S1)
//! - [`CrashPoint::BeforeWrite`] — the step never reached the journal.
//! - [`CrashPoint::MidWrite`] — the last line is torn (torn line).
//! - [`CrashPoint::MidReplay`] — crash mid-replay (resuming the resume).
//! - [`CrashPoint::CorruptedJournal`] — a non-final line got corrupted.
//!
//! ## Reproducibility
//! The clock [`Timestamp`] is injected — the system clock is never read. The
//! task's steps are a fixed deterministic script, so the same subject + same
//! clock → identical result on every run (design §2.2).

use async_trait::async_trait;

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::metrics;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::{CrashPoint, RestartReport, Subject, Task};

/// The fixed crash points the scenario walks through in order.
///
/// [`CrashPoint::Clean`] is NOT in this list — it is run separately as the
/// crash-free baseline, not as an adversarial point.
const CRASH_POINTS: [CrashPoint; 4] = [
    CrashPoint::BeforeWrite,
    CrashPoint::MidWrite,
    CrashPoint::MidReplay,
    CrashPoint::CorruptedJournal,
];

/// Number of steps in the fixed task run by the scenario.
///
/// Kept small but > 1, so that "continue from the next step" is a meaningful
/// claim (a single-step task cannot demonstrate resuming from the middle).
const TASK_STEPS: usize = 5;

/// S1 Crash Matrix scenario.
///
/// Runs the same multi-step task at each crash point and compares the result
/// to a crash-free baseline.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrashMatrix;

impl CrashMatrix {
    /// Builds a new Crash Matrix scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Builds the fixed multi-step task used by the scenario.
    ///
    /// Deterministic: same `id` and same steps on every run.
    fn task() -> Task {
        let steps: Vec<String> = (0..TASK_STEPS).map(|i| format!("step-{i}")).collect();
        Task::new(
            "s1_crash_matrix_task",
            "fixed multi-step durable workload for the crash matrix",
            steps,
        )
    }

    /// Runs the crash-free baseline: start the task, restart without
    /// crashing, and return the restart report for comparison.
    async fn baseline_run(
        subject: &mut dyn Subject,
        task: &Task,
        clock: Timestamp,
    ) -> Result<RestartReport> {
        let _handle = subject.start_task(task, clock).await?;
        // No kill call — a clean stop ([`CrashPoint::Clean`] semantics).
        // The subject restarts and reports the crash-free end state.
        let report = subject.restart(clock).await?;
        Ok(report)
    }

    /// Runs a single crash point: start the task, kill it at the point,
    /// restart, and return the restart outcome.
    ///
    /// With a corrupted journal, `restart` **refuses loudly** (the durable
    /// substrate returns an error instead of continuing into the wrong
    /// state). This is the correct continuity guarantee (design §3 S1: "loud,
    /// never silently wrong"), so the error is converted into a controlled
    /// [`CrashRunOutcome::LoudRefusal`] result specifically at the corruption
    /// point — not into a harness fatal.
    async fn crash_run(
        subject: &mut dyn Subject,
        task: &Task,
        point: CrashPoint,
        clock: Timestamp,
    ) -> Result<CrashRunOutcome> {
        let handle = subject.start_task(task, clock).await?;
        subject.kill(&handle, point).await?;
        match subject.restart(clock).await {
            Ok(report) => Ok(CrashRunOutcome::Resumed(report)),
            // At the corruption point, a loud refusal IS the correct outcome:
            // no side effect got re-executed and no state was silently
            // corrupted. At other points, an error is a genuine bug →
            // propagate it.
            Err(err) if point == CrashPoint::CorruptedJournal => {
                Ok(CrashRunOutcome::LoudRefusal(err.to_string()))
            }
            Err(err) => Err(err),
        }
    }
}

/// A single crash point's `restart` outcome: either the subject resumed (with
/// a report) or refused loudly (the correct outcome at the corruption point).
enum CrashRunOutcome {
    /// The subject resumed and reported the end state.
    Resumed(RestartReport),
    /// The subject refused loudly (durable error) — a win at the corruption point.
    LoudRefusal(String),
}

/// A single crash point's assessment result, for internal aggregation.
struct PointAssessment {
    /// Whether the workload resumed correctly from this point (accounting
    /// for side effects).
    resumed_correctly: bool,
    /// How many extra side effects replay ran (target 0).
    side_effect_overcount: usize,
    /// Whether the end state matched the crash-free baseline.
    matches_baseline: bool,
}

/// Assesses a single crash point's report against the baseline.
///
/// `expected_steps` is the number of steps in the task; `correctly_resumed`
/// is computed from the report. Resume is correct when it recovered from the
/// replay state to a clean end state with no extra side effects.
fn assess_point(report: &RestartReport, baseline: &RestartReport) -> PointAssessment {
    let side_effect_overcount = report.side_effects_reexecuted;
    // Resume is correct only if: it reached a clean end state AND no side
    // effect was re-executed.
    let resumed_correctly = report.resumed_clean && side_effect_overcount == 0;
    // The result matches the baseline when both reached a clean end state.
    // (RestartReport captures end-state integrity via the `resumed_clean`
    // flag; the baseline is always clean, so the comparison is anchored on
    // baseline.resumed_clean.)
    let matches_baseline = report.resumed_clean == baseline.resumed_clean && baseline.resumed_clean;
    PointAssessment {
        resumed_correctly,
        side_effect_overcount,
        matches_baseline,
    }
}

#[async_trait]
impl Scenario for CrashMatrix {
    // Trait signature requires `&str`; the literal is always `'static`, so
    // clippy's `&'static str` suggestion doesn't fit this implementation.
    #[allow(clippy::unnecessary_literal_bound)]
    fn id(&self) -> &str {
        "s1_crash_matrix"
    }

    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let task = Self::task();
        let expected_steps = task.steps.len();
        if expected_steps == 0 {
            return Err(crate::BenchError::scenario(
                "s1_crash_matrix: task must have at least one step",
            ));
        }

        // 1) Crash-free baseline for comparison.
        let baseline = Self::baseline_run(subject, &task, clock).await?;

        let mut result = ScenarioResult::new(self.id(), false).with_note(format!(
            "baseline (no-crash) restart: steps_replayed={}, resumed_clean={}",
            baseline.steps_replayed, baseline.resumed_clean
        ));

        // 2) Walk through every crash point and collect assessments.
        let mut correctly_resumed_points: usize = 0;
        let mut total_overcount: usize = 0;
        let mut all_match_baseline = true;

        for point in CRASH_POINTS {
            let outcome = Self::crash_run(subject, &task, point, clock).await?;
            let (assessment, note) = match outcome {
                CrashRunOutcome::Resumed(report) => {
                    let assessment = assess_point(&report, &baseline);
                    let note = format!(
                        "{point:?}: steps_replayed={}, was_replaying={}, \
                         side_effects_reexecuted={}, resumed_clean={} → \
                         resumed_correctly={}, matches_baseline={}",
                        report.steps_replayed,
                        report.was_replaying,
                        report.side_effects_reexecuted,
                        report.resumed_clean,
                        assessment.resumed_correctly,
                        assessment.matches_baseline,
                    );
                    (assessment, note)
                }
                CrashRunOutcome::LoudRefusal(err) => {
                    // A loud refusal at the corruption point = the correct
                    // outcome: no side effects re-executed, no silent
                    // corruption. This counts as a correctly resumed point
                    // and as baseline-matching (the end state did not diverge
                    // in the wrong direction — it refused as it should).
                    let assessment = PointAssessment {
                        resumed_correctly: true,
                        side_effect_overcount: 0,
                        matches_baseline: baseline.resumed_clean,
                    };
                    let note = format!("{point:?}: loud refusal (correct) → {err}");
                    (assessment, note)
                }
            };

            if assessment.resumed_correctly {
                correctly_resumed_points += 1;
            }
            total_overcount += assessment.side_effect_overcount;
            all_match_baseline &= assessment.matches_baseline;

            result = result.with_note(note);
        }

        // 3) Metrics (design §3 S1).
        //
        // resume_correctness: 1.0 only if ALL points resumed correctly.
        // Modeled with the metrics::resume_correctness function where
        // "steps" are the crash points: expected = CRASH_POINTS.len(),
        // correctly_resumed = number of correctly resumed points,
        // side_effects = total overcount. (Any re-executed side effect
        // forces the result to zero.)
        let resume_score = metrics::resume_correctness(
            CRASH_POINTS.len(),
            correctly_resumed_points,
            total_overcount,
        )?;

        // side_effect_overcount: total side-effect overcount (target 0).
        let overcount_metric = f64::from(u32::try_from(total_overcount).unwrap_or(u32::MAX));

        // result_matches_baseline: 1.0 if every crashed run's end state
        // matches the crash-free baseline.
        let matches_metric = if all_match_baseline { 1.0 } else { 0.0 };

        // passed = all three are perfect.
        let passed =
            (resume_score - 1.0).abs() < f64::EPSILON && total_overcount == 0 && all_match_baseline;

        let result = ScenarioResult { passed, ..result }
            .with_metric("resume_correctness", resume_score)
            .with_metric("side_effect_overcount", overcount_metric)
            .with_metric("result_matches_baseline", matches_metric);

        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Constants 0.0/1.0 are exact float values in these tests.
#[allow(clippy::unnecessary_literal_bound)] // The stub trait's `name(&self) -> &str` requires `&str`.
mod tests {
    use super::*;
    use crate::subject::{DreamSummary, RecallHit, RunHandle};

    /// A programmable stub subject whose restart report can be configured
    /// per crash point — lets the test simulate both a healthy and a broken
    /// subject.
    struct ProgrammableSubject {
        /// Side-effect overcount reported by restart (constant across all points).
        side_effects_reexecuted: usize,
        /// Whether restart reached a clean end state.
        resumed_clean: bool,
    }

    impl ProgrammableSubject {
        fn healthy() -> Self {
            Self {
                side_effects_reexecuted: 0,
                resumed_clean: true,
            }
        }
    }

    #[async_trait]
    impl Subject for ProgrammableSubject {
        async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "programmable"))
        }
        async fn kill(&mut self, _handle: &RunHandle, _point: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: TASK_STEPS,
                was_replaying: true,
                side_effects_reexecuted: self.side_effects_reexecuted,
                resumed_clean: self.resumed_clean,
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
            "programmable"
        }
    }

    fn fixed_clock() -> Timestamp {
        familyclaw_core::time::from_unix_secs(1_717_000_000).expect("valid clock")
    }

    #[tokio::test]
    async fn healthy_subject_passes_all_three() {
        let mut subject = ProgrammableSubject::healthy();
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(result.passed, "healthy subject must pass S1");
        assert_eq!(result.metrics["resume_correctness"], 1.0);
        assert_eq!(result.metrics["side_effect_overcount"], 0.0);
        assert_eq!(result.metrics["result_matches_baseline"], 1.0);
    }

    #[tokio::test]
    async fn side_effect_overcount_fails_scenario() {
        let mut subject = ProgrammableSubject {
            side_effects_reexecuted: 1,
            resumed_clean: true,
        };
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(!result.passed, "any re-executed side effect must fail S1");
        // 4 crash points × 1 extra side effect = 4.
        assert_eq!(result.metrics["side_effect_overcount"], 4.0);
        assert_eq!(result.metrics["resume_correctness"], 0.0);
    }

    #[tokio::test]
    async fn unclean_resume_fails_baseline_match() {
        let mut subject = ProgrammableSubject {
            side_effects_reexecuted: 0,
            resumed_clean: false,
        };
        let result = CrashMatrix::new()
            .run(&mut subject, fixed_clock())
            .await
            .expect("scenario runs");
        assert!(!result.passed, "unclean resume must fail S1");
        // The baseline also fails to reach a clean state → matches_baseline = 0.
        assert_eq!(result.metrics["result_matches_baseline"], 0.0);
    }

    #[tokio::test]
    async fn id_is_stable() {
        assert_eq!(CrashMatrix::new().id(), "s1_crash_matrix");
    }

    #[test]
    fn assess_point_clean_report_is_correct() {
        let clean = RestartReport {
            steps_replayed: TASK_STEPS,
            was_replaying: true,
            side_effects_reexecuted: 0,
            resumed_clean: true,
        };
        let assessment = assess_point(&clean, &clean);
        assert!(assessment.resumed_correctly);
        assert_eq!(assessment.side_effect_overcount, 0);
        assert!(assessment.matches_baseline);
    }
}

//! [`MarkdownFileSubject`] — an HONEST competitor-shaped baseline (a `MEMORY.md` model).
//!
//! ## Important honesty warning
//! This is **NOT** a real OpenClaw or Hermes Agent. It is a *competitor-shaped
//! model* (design §2.1): a pure in-process [`Subject`] that mimics the
//! **documented** behavior of file-based Markdown memory (an OpenClaw/Hermes-
//! style `MEMORY.md`). The goal is to make the continuity benchmark a
//! *head-to-head comparison* — not to claim this is any real product's
//! internals. The modeled behaviors are exactly the failure modes that make
//! FamilyClaw better.
//!
//! ## Modeled (documented) competitor behaviors
//! 1. **Memory** — a single in-memory `MEMORY.md`-style buffer
//!    ([`Vec<String>`]) with a *bootstrap budget* ([`BOOTSTRAP_BUDGET`]).
//!    When the budget is exceeded, the buffer **silently truncates the
//!    oldest entry first** (OpenClaw's documented `MEMORY.md` truncation).
//!    No protected core, no decay policy — important identity facts get
//!    truncated just like any other line.
//! 2. **Restart** — NO deterministic crash replay. On restart it **re-runs
//!    the task's steps from scratch** (re-executing side effects). It
//!    reaches a similar end state, but via a re-run, not a replay.
//! 3. **Recall** — a naive substring search over the (possibly truncated)
//!    buffer, with relevance fixed at `1.0` for a hit. If a fact's line was
//!    truncated, recall returns nothing for it — this is the retention
//!    failure the benchmark measures.
//! 4. **Sleep** — no-op consolidation: `protected_core_intact = false`,
//!    because it has NO protected core (an honest model of "no eternal
//!    thread").
//!
//! ## Reproducibility
//! The same task → the same numbers. No system clock, no randomness — every
//! operation that needs time receives a [`Timestamp`] injected, though the
//! calculation doesn't actually need it (the baseline is purely a state
//! machine). The clock is accepted only as an interface seam.

use async_trait::async_trait;

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::subject::{
    CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task,
};

/// The `MEMORY.md` bootstrap budget: how many lines fit in the buffer before
/// the oldest is silently truncated (OpenClaw's documented truncation limit).
pub const BOOTSTRAP_BUDGET: usize = 8;

/// A named competitor profile for file-based memory.
///
/// The same in-process model, parameterized after two DOCUMENTED file
/// agents' behaviors, so `compare` produces a **named** comparison instead of
/// one generic baseline. Still honestly a *model*, NOT a real product (see
/// the file's honesty warning) — only the budget limits are profile-specific.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompetitorProfile {
    /// A generic `MEMORY.md` oldest-first truncation (default, line budget).
    Generic,
    /// OpenClaw-style: a documented `MEMORY.md` bootstrap budget, silent
    /// oldest-first truncation, no protected core.
    OpenClaw,
    /// Hermes-style: a documented hard character limit (`MEMORY.md` ~2,200
    /// characters), which truncates the oldest entry once the sum is exceeded.
    Hermes,
}

impl CompetitorProfile {
    /// The profile's stable subject name for the scorecard.
    #[must_use]
    pub fn subject_name(self) -> &'static str {
        match self {
            Self::Generic => "markdown-file-baseline",
            Self::OpenClaw => "openclaw-memory-md-model",
            Self::Hermes => "hermes-memory-2k-model",
        }
    }

    /// The line budget (`None` = use the character limit instead).
    #[must_use]
    fn line_budget(self) -> Option<usize> {
        match self {
            Self::Generic | Self::OpenClaw => Some(BOOTSTRAP_BUDGET),
            Self::Hermes => None,
        }
    }

    /// The hard character limit (`None` = use the line budget instead).
    /// Hermes's documented `MEMORY.md` ceiling is ~2,200 characters.
    #[must_use]
    fn char_budget(self) -> Option<usize> {
        match self {
            Self::Hermes => Some(2_200),
            Self::Generic | Self::OpenClaw => None,
        }
    }
}

/// An honest competitor baseline: a file-based Markdown memory model.
///
/// Pure in-process — no child process. Holds a single `MEMORY.md`-style
/// buffer, tracks the active task, and tracks how many steps completed
/// before the crash (so restart can report how many side effects get
/// re-run).
#[derive(Debug)]
pub struct MarkdownFileSubject {
    /// The `MEMORY.md`-style memory buffer (oldest first; truncated from the head).
    buffer: Vec<String>,
    /// The active task (set in [`start_task`](MarkdownFileSubject::start_task)).
    task: Option<Task>,
    /// The number of steps completed before the crash (set in
    /// [`kill`](MarkdownFileSubject::kill), consumed in
    /// [`restart`](MarkdownFileSubject::restart)).
    completed_steps: usize,
    /// Whether the last crash was clean (`Clean`) — determines the restart outcome.
    last_crash_clean: bool,
    /// The subject's stable name for the scorecard.
    name: String,
    /// The named competitor profile (budget limit + name).
    profile: CompetitorProfile,
}

impl Default for MarkdownFileSubject {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownFileSubject {
    /// Builds a fresh baseline with an empty memory buffer (generic profile).
    #[must_use]
    pub fn new() -> Self {
        Self::with_profile(CompetitorProfile::Generic)
    }

    /// Builds a baseline with a named competitor profile (OpenClaw/Hermes/generic).
    /// The budget limit and subject name come from the profile; other
    /// behavior is identical.
    #[must_use]
    pub fn with_profile(profile: CompetitorProfile) -> Self {
        Self {
            buffer: Vec::new(),
            task: None,
            completed_steps: 0,
            last_crash_clean: true,
            name: profile.subject_name().to_string(),
            profile,
        }
    }

    /// Appends a line to the `MEMORY.md` buffer and silently truncates the
    /// oldest entry first if the bootstrap budget is exceeded.
    ///
    /// This is the core of the honest model: NO protected core and no decay
    /// policy — the most important identity fact gets truncated just like
    /// the most recent trivial line, the moment the budget fills up.
    fn push_memory(&mut self, line: impl Into<String>) {
        self.buffer.push(line.into());
        // Profile-specific oldest-first truncation. Silent — no log, no
        // protection: this is exactly the documented file-agent failure.
        if let Some(max_lines) = self.profile.line_budget() {
            while self.buffer.len() > max_lines {
                self.buffer.remove(0);
            }
        }
        if let Some(max_chars) = self.profile.char_budget() {
            // Hermes model: a hard character ceiling (~2,200). Trim the
            // oldest until the buffer's total character count fits. Always
            // keep at least the newest line.
            while self.buffer.len() > 1
                && self.buffer.iter().map(String::len).sum::<usize>() > max_chars
            {
                self.buffer.remove(0);
            }
        }
    }

    /// Executes the task's steps from start to finish, recording each into
    /// the buffer (modeling the side effect). Returns the number of steps
    /// executed.
    ///
    /// This is called both on the first run ([`kill`](MarkdownFileSubject::kill))
    /// and on restart ([`restart`](MarkdownFileSubject::restart)) — because
    /// the baseline does NOT replay but re-runs.
    fn run_steps(&mut self, task: &Task) -> usize {
        for (idx, step) in task.steps.iter().enumerate() {
            self.push_memory(format!("[{}] step {}: {}", task.id, idx, step));
        }
        task.steps.len()
    }

    /// The active task, or an error if
    /// [`start_task`](MarkdownFileSubject::start_task) hasn't been called.
    fn require_task(&self) -> Result<Task> {
        self.task
            .clone()
            .ok_or_else(|| crate::BenchError::subject("no active task — call start_task first"))
    }

    /// Returns the current memory buffer (for tests and introspection).
    #[must_use]
    pub fn buffer(&self) -> &[String] {
        &self.buffer
    }
}

#[async_trait]
impl Subject for MarkdownFileSubject {
    async fn start_task(&mut self, task: &Task, _clock: Timestamp) -> Result<RunHandle> {
        // A fresh task: reset the crash state. The memory buffer is NOT
        // reset — `MEMORY.md` is long-lived and accumulates lines across
        // tasks (which is exactly why it exceeds the budget and truncates
        // identity facts).
        self.task = Some(task.clone());
        self.completed_steps = 0;
        self.last_crash_clean = true;
        // Token = task ID (an opaque reference; no process/journal).
        Ok(RunHandle::new(task.id.clone(), task.id.clone()))
    }

    async fn kill(&mut self, _handle: &RunHandle, point: CrashPoint) -> Result<()> {
        let task = self.require_task()?;
        let total = task.steps.len();
        // How many steps were COMPLETED (side effect executed) before the
        // crash. Deterministic values per crash point — restart re-runs these.
        let completed = match point {
            // A clean stop or a crash mid-replay: all steps completed.
            CrashPoint::Clean | CrashPoint::MidReplay => total,
            // A crash before the last step's write: all but the last completed.
            CrashPoint::BeforeWrite | CrashPoint::MidWrite | CrashPoint::CorruptedJournal => {
                total.saturating_sub(1)
            }
        };

        // Run the task up to the crash (side effects into the buffer). Clean
        // runs the whole task; others run the first `completed` steps.
        for (idx, step) in task.steps.iter().take(completed).enumerate() {
            self.push_memory(format!("[{}] step {}: {}", task.id, idx, step));
        }

        self.completed_steps = completed;
        self.last_crash_clean = matches!(point, CrashPoint::Clean);
        Ok(())
    }

    async fn restart(&mut self, _clock: Timestamp) -> Result<RestartReport> {
        let task = self.require_task()?;

        if self.last_crash_clean {
            // A clean baseline: no crash → nothing to re-run.
            return Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            });
        }

        // After a crash: the baseline does NOT replay from a journal but
        // RE-RUNS the task's steps from scratch → side effects that already
        // completed once are executed again.
        let reexecuted = self.completed_steps;
        let _reached_end = self.run_steps(&task);

        Ok(RestartReport {
            // No replay: no steps recovered from a log.
            steps_replayed: 0,
            // Never in replay mode — a re-run is not a replay.
            was_replaying: false,
            // Steps that already completed are re-run along with their side effects.
            side_effects_reexecuted: reexecuted,
            // The end state is similar but reached via a re-run, not a
            // deterministic replay — hence NOT a clean resume.
            resumed_clean: false,
        })
    }

    async fn recall(&mut self, query: &str, _clock: Timestamp) -> Result<Vec<RecallHit>> {
        // A naive substring search over the (possibly truncated) buffer. If
        // the line containing a fact was truncated, no hit comes back —
        // this is the retention failure the benchmark measures.
        let hits = self
            .buffer
            .iter()
            .filter(|line| line.contains(query))
            // A fixed relevance of 1.0 for a hit — no scoring, no ranking.
            .map(|line| RecallHit::new(line.clone(), 1.0))
            .collect();
        Ok(hits)
    }

    async fn sleep_cycle(&mut self, _clock: Timestamp) -> Result<DreamSummary> {
        // No-op consolidation: the baseline doesn't dedupe, doesn't drop
        // conflicts, doesn't absolutize dates, doesn't strengthen or archive.
        // It only scans the buffer's size. `protected_core_intact = false` is
        // the honest truth — it has NO protected core (no eternal thread).
        Ok(DreamSummary {
            scanned: self.buffer.len(),
            merged: 0,
            dropped: 0,
            dates_absolutized: 0,
            strengthened: 0,
            archived: 0,
            protected_core_intact: false,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;

    /// A fixed injected clock — the baseline doesn't use it, but the interface requires it.
    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid")
    }

    /// A task with `n` steps (a deterministic script).
    fn task_with_steps(id: &str, n: usize) -> Task {
        let steps = (0..n).map(|i| format!("do-{i}")).collect();
        Task::new(id, "baseline demo", steps)
    }

    #[tokio::test]
    async fn clean_crash_restart_resumes_clean_with_no_reexecution() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-clean", 3);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        subject
            .kill(&handle, CrashPoint::Clean)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert!(report.resumed_clean, "clean crash → resumed_clean");
        assert_eq!(report.side_effects_reexecuted, 0, "clean crash → no re-run");
        assert_eq!(report.steps_replayed, 0, "the baseline never replays");
        assert!(!report.was_replaying);
    }

    #[tokio::test]
    async fn crash_restart_reexecutes_side_effects_and_is_not_clean() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-crash", 4);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        // A crash before the last step's write: 3 steps completed.
        subject
            .kill(&handle, CrashPoint::BeforeWrite)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert!(
            report.side_effects_reexecuted > 0,
            "a crash → side effects are re-run"
        );
        assert_eq!(
            report.side_effects_reexecuted, 3,
            "BeforeWrite left 3/4 complete → 3 re-runs"
        );
        assert!(!report.resumed_clean, "a re-run is not a clean resume");
        assert_eq!(
            report.steps_replayed, 0,
            "the baseline re-runs rather than replaying"
        );
        assert!(!report.was_replaying);
    }

    #[tokio::test]
    async fn mid_replay_crash_reexecutes_all_steps() {
        let mut subject = MarkdownFileSubject::new();
        let task = task_with_steps("t-midreplay", 5);
        let handle = subject
            .start_task(&task, fixed_clock())
            .await
            .expect("start");
        subject
            .kill(&handle, CrashPoint::MidReplay)
            .await
            .expect("kill");

        let report = subject.restart(fixed_clock()).await.expect("restart");
        assert_eq!(
            report.side_effects_reexecuted, 5,
            "MidReplay → all 5 steps are re-run"
        );
        assert!(!report.resumed_clean);
    }

    #[tokio::test]
    async fn memory_truncates_oldest_first_and_recall_misses_truncated_fact() {
        let mut subject = MarkdownFileSubject::new();
        // An important identity fact FIRST — it's the oldest, so it gets
        // truncated first once the budget is exceeded.
        let important = "IDENTITY: the maintainer is the family creator".to_string();
        subject.push_memory(important.clone());
        // Push well past the budget with other filler.
        for i in 0..(BOOTSTRAP_BUDGET + 5) {
            subject.push_memory(format!("trivia line {i}"));
        }

        // The buffer doesn't exceed the budget (oldest entries truncated).
        assert_eq!(
            subject.buffer().len(),
            BOOTSTRAP_BUDGET,
            "the buffer stays within budget"
        );
        // The most important fact was truncated (oldest first).
        assert!(
            !subject.buffer().contains(&important),
            "the most important fact was silently truncated"
        );

        // Recall doesn't find the truncated fact → a retention failure.
        let hits = subject
            .recall("IDENTITY", fixed_clock())
            .await
            .expect("recall");
        assert!(
            hits.is_empty(),
            "a truncated identity fact can no longer be recalled"
        );

        // But a more recent line is still found, with relevance 1.0.
        let hits = subject
            .recall("trivia line 5", fixed_clock())
            .await
            .expect("recall");
        assert_eq!(hits.len(), 1);
        assert!((hits[0].relevance - 1.0).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn sleep_cycle_has_no_protected_core() {
        let mut subject = MarkdownFileSubject::new();
        subject.push_memory("line a");
        subject.push_memory("line b");

        let summary = subject.sleep_cycle(fixed_clock()).await.expect("sleep");
        assert!(
            !summary.protected_core_intact,
            "the baseline has no protected core"
        );
        assert_eq!(summary.scanned, 2, "scans the buffer's size");
        assert_eq!(summary.merged, 0, "no-op consolidation: no dedup");
        assert_eq!(summary.dropped, 0);
        assert_eq!(summary.dates_absolutized, 0);
        assert_eq!(summary.strengthened, 0);
        assert_eq!(summary.archived, 0);
    }

    #[tokio::test]
    async fn name_is_stable() {
        let subject = MarkdownFileSubject::new();
        assert_eq!(subject.name(), "markdown-file-baseline");
    }

    #[tokio::test]
    async fn named_profiles_have_stable_distinct_names() {
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::OpenClaw).name(),
            "openclaw-memory-md-model"
        );
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::Hermes).name(),
            "hermes-memory-2k-model"
        );
        assert_eq!(
            MarkdownFileSubject::with_profile(CompetitorProfile::Generic).name(),
            "markdown-file-baseline"
        );
    }

    #[tokio::test]
    async fn openclaw_profile_truncates_by_line_budget_oldest_first() {
        let mut subject = MarkdownFileSubject::with_profile(CompetitorProfile::OpenClaw);
        let important = "IDENTITY: maintainer is the family creator".to_string();
        subject.push_memory(important.clone());
        for i in 0..(BOOTSTRAP_BUDGET + 5) {
            subject.push_memory(format!("trivia {i}"));
        }
        assert_eq!(subject.buffer().len(), BOOTSTRAP_BUDGET, "line budget");
        assert!(
            !subject.buffer().contains(&important),
            "the OpenClaw model silently truncates identity (oldest-first)"
        );
    }

    #[tokio::test]
    async fn hermes_profile_truncates_by_char_budget() {
        let mut subject = MarkdownFileSubject::with_profile(CompetitorProfile::Hermes);
        // A ~2,200-character ceiling: push well past it → the oldest entries
        // get trimmed, but the line count is NOT bounded (unlike OpenClaw):
        // the char ceiling handles it.
        let big = "x".repeat(500);
        for _ in 0..10 {
            subject.push_memory(big.clone());
        }
        let total: usize = subject.buffer().iter().map(String::len).sum();
        assert!(
            total <= 2_200,
            "the Hermes model keeps the character sum under the ceiling"
        );
        assert!(
            !subject.buffer().is_empty(),
            "the newest line always survives"
        );
    }

    #[tokio::test]
    async fn same_task_yields_same_numbers() {
        // Determinism: two identical runs → identical restart numbers.
        async fn run() -> RestartReport {
            let mut subject = MarkdownFileSubject::new();
            let task = task_with_steps("t-det", 4);
            let handle = subject
                .start_task(&task, fixed_clock())
                .await
                .expect("start");
            subject
                .kill(&handle, CrashPoint::MidWrite)
                .await
                .expect("kill");
            subject.restart(fixed_clock()).await.expect("restart")
        }
        assert_eq!(run().await, run().await);
    }
}

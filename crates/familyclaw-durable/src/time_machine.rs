//! Time Machine — journal inspection (inspect), branching (fork), and
//! timeline comparison (diff).
//!
//! A durable journal is append-only and replay is deterministic — which
//! means history is not only *replayable* but also *forkable*: any past
//! decision point can be opened, cut, and re-run with modified continuation
//! logic without the original timeline changing or a single real side effect
//! recurring.
//!
//! Three layers:
//!
//! 1. **Inspect** — [`Timeline`] unpacks the journal into a human-readable
//!    list of steps: what happened, in what order, what succeeded and what
//!    failed (a "black box" read).
//! 2. **Fork** — [`TimeMachine::fork`] copies the start of a timeline into a
//!    new journal and truncates it at the chosen step. In the fork, history
//!    replays deterministically up to the cut point, and from there on
//!    execution is *fresh* — the caller can run an alternative continuation
//!    ("what if?"). A [`FORK_MARKER`] audit row is always recorded at the
//!    start of the fork, so the origin of a forked timeline is verifiable.
//! 3. **Diff** — [`TimelineDiff`] compares two timelines step by step and
//!    produces a deterministic, serializable report: what stayed the same,
//!    what result changed, where the timelines diverged.
//!
//! For counterfactual runs, [`DryRunRecorder`] captures *intended* external
//! side effects as intents. The type **has structurally no dispatch path
//! whatsoever** — a captured intent can never reach an external system
//! through this type. This is the same fail-closed principle used
//! throughout the platform: safety is a property of structure, not policy.
//!
//! ## Example: rewind, fork, diff
//! ```
//! use familyclaw_durable::{DurableContext, InMemoryJournal, TimeMachine};
//!
//! # fn main() -> familyclaw_durable::Result<()> {
//! // Original run: two steps.
//! let mut ctx = DurableContext::new(InMemoryJournal::new())?;
//! let a: i64 = ctx.step("load", || Ok(10))?;
//! let _b: i64 = ctx.step("apply", || Ok(a * 2))?;
//! let original = ctx.finish();
//!
//! // Fork: keep only "load", run "apply" with new logic.
//! let fork = TimeMachine::fork(&original, 1)?;
//! let mut alt = DurableContext::new(fork)?;
//! let a: i64 = alt.step("load", || Ok(0))?; // replay: restored from the log (10)
//! let _b: i64 = alt.step("apply", || Ok(a * 3))?; // fresh: new logic
//! let forked = alt.finish();
//!
//! // Compare timelines: "load" unchanged, "apply" changed 20 → 30.
//! let diff = TimeMachine::diff(&original, &forked)?;
//! assert!(!diff.is_identical());
//! assert_eq!(diff.changed_count(), 1);
//! # Ok(())
//! # }
//! ```

use std::fmt::Write as _;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;

use crate::entry::{EntryKind, JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;
use crate::memory::InMemoryJournal;

/// Name of the audit marker recorded at the start of every forked timeline
/// (more precisely: right after the copied prefix).
///
/// The payload records how many steps were kept in the prefix and how many
/// the source timeline had in total — so the origin of a forked timeline is
/// always verifiable from the log itself.
pub const FORK_MARKER: &str = "timeline_forked";

/// Outcome of a single workflow step in the inspect and diff views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StepOutcome {
    /// The step completed; `output` is the result stored in the journal.
    Completed {
        /// The step's returned result as a JSON value.
        output: serde_json::Value,
    },
    /// The step failed; `error` is the stored error message.
    Failed {
        /// The stored error message.
        error: String,
    },
}

impl StepOutcome {
    /// Whether the outcome is a success.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, StepOutcome::Completed { .. })
    }
}

/// A single workflow step in the timeline inspection view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineStep {
    /// The step's 0-based position among the timeline's **steps**
    /// (markers and snapshots do not consume positions — the same cursor
    /// convention as [`crate::DurableContext`] replay).
    pub position: usize,
    /// The step's sequence position in the journal.
    pub step_id: StepId,
    /// The step's logical name.
    pub name: String,
    /// The step's outcome.
    pub outcome: StepOutcome,
    /// The moment the row was written (diagnostics only — does not affect
    /// determinism).
    pub timestamp: Timestamp,
}

/// A read-only, immutable inspection view of a journal (a "black box").
///
/// Built via [`Timeline::from_journal`]. Contains only the workflow steps in
/// order; marker and snapshot counts are reported separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Timeline {
    /// The workflow steps in append order.
    pub steps: Vec<TimelineStep>,
    /// Number of marker rows in the log (including session state).
    pub marker_count: usize,
    /// Number of snapshot rows in the log.
    pub snapshot_count: usize,
}

impl Timeline {
    /// Reads the journal and builds an inspection view.
    ///
    /// # Errors
    /// Propagates the journal's read error ([`DurableError::Io`],
    /// [`DurableError::CorruptEntry`], ...).
    pub fn from_journal<J: Journal>(journal: &J) -> Result<Self> {
        let mut steps = Vec::new();
        let mut marker_count = 0usize;
        let mut snapshot_count = 0usize;

        for entry in journal.replay_all()? {
            match &entry.kind {
                EntryKind::StepCompleted { name, output } => {
                    steps.push(TimelineStep {
                        position: steps.len(),
                        step_id: entry.step_id,
                        name: name.clone(),
                        outcome: StepOutcome::Completed {
                            output: output.clone(),
                        },
                        timestamp: entry.timestamp,
                    });
                }
                EntryKind::StepFailed { name, error } => {
                    steps.push(TimelineStep {
                        position: steps.len(),
                        step_id: entry.step_id,
                        name: name.clone(),
                        outcome: StepOutcome::Failed {
                            error: error.clone(),
                        },
                        timestamp: entry.timestamp,
                    });
                }
                kind if kind.is_snapshot() => snapshot_count += 1,
                _ => marker_count += 1,
            }
        }

        Ok(Self {
            steps,
            marker_count,
            snapshot_count,
        })
    }

    /// Number of steps in the timeline.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether the timeline is empty (no workflow steps at all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the step at the given position, if one exists.
    #[must_use]
    pub fn step(&self, position: usize) -> Option<&TimelineStep> {
        self.steps.get(position)
    }

    /// Finds the first step with the given name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&TimelineStep> {
        self.steps.iter().find(|s| s.name == name)
    }

    /// Human-readable markdown report of the timeline (for CLI/reporting use).
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Timeline — {} step(s), {} marker(s), {} snapshot(s)\n",
            self.steps.len(),
            self.marker_count,
            self.snapshot_count
        );
        let _ = writeln!(out, "| # | step | outcome |");
        let _ = writeln!(out, "|---|------|---------|");
        for step in &self.steps {
            let outcome = match &step.outcome {
                StepOutcome::Completed { output } => format!("ok: `{output}`"),
                StepOutcome::Failed { error } => format!("FAILED: {error}"),
            };
            let _ = writeln!(out, "| {} | `{}` | {} |", step.position, step.name, outcome);
        }
        out
    }
}

/// Comparison result for a single step pair between two timelines.
///
/// `before` is the left-hand (original) side of the comparison and `after`
/// is the right-hand (e.g. forked) timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepDiff {
    /// Same step, same outcome on both timelines.
    Unchanged {
        /// The step's position on both timelines.
        position: usize,
        /// The step's name.
        name: String,
    },
    /// Same step name, but the outcome changed (result or success/failure).
    Changed {
        /// The step's position on both timelines.
        position: usize,
        /// The step's name.
        name: String,
        /// The outcome on the original timeline.
        before: StepOutcome,
        /// The outcome on the compared timeline.
        after: StepOutcome,
    },
    /// The timelines diverged: a different step (different name) occupies
    /// the same position.
    Diverged {
        /// The position where divergence was detected.
        position: usize,
        /// The step name on the original timeline.
        before_name: String,
        /// The step name on the compared timeline.
        after_name: String,
    },
    /// The step exists only on the original timeline (the compared one is
    /// shorter).
    OnlyInBefore {
        /// The step's position on the original timeline.
        position: usize,
        /// The step's name.
        name: String,
    },
    /// The step exists only on the compared timeline (the original one is
    /// shorter).
    OnlyInAfter {
        /// The step's position on the compared timeline.
        position: usize,
        /// The step's name.
        name: String,
    },
}

/// A deterministic, serializable comparison report between two timelines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineDiff {
    /// Per-step comparison results in positional order.
    pub steps: Vec<StepDiff>,
}

impl TimelineDiff {
    /// Compares two already-read timelines step by step.
    #[must_use]
    pub fn from_timelines(before: &Timeline, after: &Timeline) -> Self {
        let mut steps = Vec::new();
        let shared = before.len().min(after.len());

        for position in 0..shared {
            let b = &before.steps[position];
            let a = &after.steps[position];
            if b.name != a.name {
                steps.push(StepDiff::Diverged {
                    position,
                    before_name: b.name.clone(),
                    after_name: a.name.clone(),
                });
            } else if b.outcome == a.outcome {
                steps.push(StepDiff::Unchanged {
                    position,
                    name: b.name.clone(),
                });
            } else {
                steps.push(StepDiff::Changed {
                    position,
                    name: b.name.clone(),
                    before: b.outcome.clone(),
                    after: a.outcome.clone(),
                });
            }
        }

        for step in &before.steps[shared..] {
            steps.push(StepDiff::OnlyInBefore {
                position: step.position,
                name: step.name.clone(),
            });
        }
        for step in &after.steps[shared..] {
            steps.push(StepDiff::OnlyInAfter {
                position: step.position,
                name: step.name.clone(),
            });
        }

        Self { steps }
    }

    /// Whether the timelines are identical (every step is
    /// [`StepDiff::Unchanged`]).
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.steps
            .iter()
            .all(|d| matches!(d, StepDiff::Unchanged { .. }))
    }

    /// How many steps changed ([`StepDiff::Changed`]).
    #[must_use]
    pub fn changed_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|d| matches!(d, StepDiff::Changed { .. }))
            .count()
    }

    /// How many steps exist on only one of the timelines
    /// ([`StepDiff::OnlyInBefore`] + [`StepDiff::OnlyInAfter`]).
    #[must_use]
    pub fn tail_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|d| {
                matches!(
                    d,
                    StepDiff::OnlyInBefore { .. } | StepDiff::OnlyInAfter { .. }
                )
            })
            .count()
    }

    /// The first position where the timelines diverged by name, if any.
    #[must_use]
    pub fn first_divergence(&self) -> Option<usize> {
        self.steps.iter().find_map(|d| match d {
            StepDiff::Diverged { position, .. } => Some(*position),
            _ => None,
        })
    }

    /// Human-readable markdown report of the comparison.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Timeline diff — {} step(s): {} changed, {} tail-only, identical: {}\n",
            self.steps.len(),
            self.changed_count(),
            self.tail_count(),
            self.is_identical()
        );
        for diff in &self.steps {
            let line = match diff {
                StepDiff::Unchanged { position, name } => {
                    format!("- `#{position}` `{name}` — unchanged")
                }
                StepDiff::Changed {
                    position,
                    name,
                    before,
                    after,
                } => format!(
                    "- `#{position}` `{name}` — **changed**: {} → {}",
                    render_outcome(before),
                    render_outcome(after)
                ),
                StepDiff::Diverged {
                    position,
                    before_name,
                    after_name,
                } => format!("- `#{position}` — **diverged**: `{before_name}` vs `{after_name}`"),
                StepDiff::OnlyInBefore { position, name } => {
                    format!("- `#{position}` `{name}` — only in BEFORE")
                }
                StepDiff::OnlyInAfter { position, name } => {
                    format!("- `#{position}` `{name}` — only in AFTER")
                }
            };
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

/// Short text representation of an outcome for the diff report.
fn render_outcome(outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::Completed { output } => format!("ok `{output}`"),
        StepOutcome::Failed { error } => format!("FAILED ({error})"),
    }
}

/// An intended external side effect captured by a counterfactual run (an
/// intent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedIntent {
    /// The logical name of the step within which the intent arose.
    pub step: String,
    /// The intent's payload (e.g. what *would have* been sent).
    pub payload: serde_json::Value,
}

/// Dry-run intent recorder for counterfactual runs.
///
/// A continuation run on a forked timeline calls the [`record`](Self::record)
/// method at the point where real execution would dispatch an external side
/// effect. **This type has no dispatch method and no path whatsoever to an
/// external system** — a captured intent can only be read and reported. This
/// structurally separates "what the agent would have done" from it ever
/// actually happening.
#[derive(Debug, Default)]
pub struct DryRunRecorder {
    intents: Mutex<Vec<RecordedIntent>>,
}

impl DryRunRecorder {
    /// Creates an empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Captures a single intended side effect.
    pub fn record(&self, step: impl Into<String>, payload: serde_json::Value) {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RecordedIntent {
                step: step.into(),
                payload,
            });
    }

    /// Returns a copy of all captured intents in capture order.
    #[must_use]
    pub fn intents(&self) -> Vec<RecordedIntent> {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The number of captured intents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the recorder is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Time Machine facade: inspect, fork, and diff under one name.
///
/// All operations are **read-only with respect to the source journal** —
/// nothing ever mutates an existing timeline (the append-only invariant is
/// preserved).
#[derive(Debug, Clone, Copy)]
pub struct TimeMachine;

impl TimeMachine {
    /// Reads the journal into an inspection view. See
    /// [`Timeline::from_journal`].
    ///
    /// # Errors
    /// Propagates the journal's read error.
    pub fn inspect<J: Journal>(journal: &J) -> Result<Timeline> {
        Timeline::from_journal(journal)
    }

    /// Forks a timeline: copies the first `keep_steps` **workflow steps**
    /// (along with the markers/snapshots preceding them) from the source
    /// journal into a new in-memory journal, then records a [`FORK_MARKER`]
    /// audit row after them.
    ///
    /// A [`crate::DurableContext`] built on top of the fork replays the
    /// retained prefix deterministically (without running side effects) and
    /// continues fresh from there — an alternative continuation can be run
    /// from the cut point onward. The source journal is not modified.
    ///
    /// # Errors
    /// [`DurableError::InvalidFork`] if `keep_steps` exceeds the source
    /// timeline's step count. Otherwise propagates the journal's
    /// read/write error.
    pub fn fork<J: Journal>(source: &J, keep_steps: usize) -> Result<InMemoryJournal> {
        let target = InMemoryJournal::new();
        Self::fork_into(source, keep_steps, &target)?;
        Ok(target)
    }

    /// Like [`fork`](Self::fork), but writes the fork into the given
    /// **empty** target journal (e.g. a [`crate::FileJournal`] for
    /// persistence). Returns the number of steps kept.
    ///
    /// # Errors
    /// [`DurableError::InvalidFork`] if the target is not empty or
    /// `keep_steps` exceeds the source timeline's step count. Otherwise
    /// propagates the journal's read/write error.
    pub fn fork_into<J: Journal, T: Journal>(
        source: &J,
        keep_steps: usize,
        target: &T,
    ) -> Result<usize> {
        if !target.is_empty()? {
            return Err(DurableError::invalid_fork(
                "fork target journal must be empty",
            ));
        }

        let all = source.replay_all()?;
        let total_steps = all.iter().filter(|e| e.kind.is_step()).count();
        if keep_steps > total_steps {
            return Err(DurableError::invalid_fork(format!(
                "cannot keep {keep_steps} step(s): source timeline has only {total_steps}"
            )));
        }

        let mut kept = 0usize;
        for entry in all {
            if entry.kind.is_step() {
                if kept == keep_steps {
                    break;
                }
                kept += 1;
            }
            target.append(entry)?;
        }

        // Audit row: the fork's origin is verifiable from the log itself.
        target.append(JournalEntry::marker(
            StepId::new(kept as u64),
            FORK_MARKER,
            serde_json::json!({
                "kept_steps": kept,
                "source_steps": total_steps,
            }),
        ))?;

        Ok(kept)
    }

    /// Compares two timelines step by step. See
    /// [`TimelineDiff::from_timelines`].
    ///
    /// # Errors
    /// Propagates either journal's read error.
    pub fn diff<A: Journal, B: Journal>(before: &A, after: &B) -> Result<TimelineDiff> {
        let b = Timeline::from_journal(before)?;
        let a = Timeline::from_journal(after)?;
        Ok(TimelineDiff::from_timelines(&b, &a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::DurableContext;
    use serde_json::json;
    use std::cell::Cell;

    /// Helper: a three-step run (load → decide → act) where "act" captures
    /// a side effect into a counter. Returns the finished journal.
    fn three_step_run(effects: &Cell<u32>) -> InMemoryJournal {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let amount: i64 = ctx.step("load", || Ok(100)).expect("load");
        let approved: i64 = ctx.step("decide", || Ok(amount * 2)).expect("decide");
        let _receipt: String = ctx
            .step("act", || {
                effects.set(effects.get() + 1);
                Ok(format!("sent:{approved}"))
            })
            .expect("act");
        ctx.finish()
    }

    // ---------- Inspect ----------

    #[test]
    fn timeline_lists_steps_in_order_with_outcomes() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        let timeline = TimeMachine::inspect(&journal).expect("inspect");
        assert_eq!(timeline.len(), 3);
        assert!(!timeline.is_empty());

        let names: Vec<&str> = timeline.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["load", "decide", "act"]);

        // Positions are 0-based and in order.
        for (i, step) in timeline.steps.iter().enumerate() {
            assert_eq!(step.position, i);
            assert!(step.outcome.is_completed());
        }

        // Results are readable.
        match &timeline.step(1).expect("decide").outcome {
            StepOutcome::Completed { output } => assert_eq!(output, &json!(200)),
            other @ StepOutcome::Failed { .. } => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn timeline_records_failures_and_counts_non_steps() {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let _ = ctx.step::<i32, _>("boom", || Err("kaboom".to_string()));
        ctx.snapshot(&json!({"x": 1})).expect("snapshot");
        let journal = ctx.finish();
        journal
            .append(JournalEntry::marker(StepId::new(9), "note", json!({})))
            .expect("marker");

        let timeline = TimeMachine::inspect(&journal).expect("inspect");
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline.snapshot_count, 1);
        assert_eq!(timeline.marker_count, 1);
        match &timeline.find("boom").expect("boom").outcome {
            StepOutcome::Failed { error } => assert_eq!(error, "kaboom"),
            other @ StepOutcome::Completed { .. } => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn timeline_render_markdown_mentions_every_step() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let text = TimeMachine::inspect(&journal)
            .expect("inspect")
            .render_markdown();
        for name in ["load", "decide", "act"] {
            assert!(text.contains(name), "markdown must mention `{name}`");
        }
    }

    // ---------- Fork ----------

    #[test]
    fn fork_keeps_prefix_truncates_tail_and_adds_audit_marker() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        let fork = TimeMachine::fork(&journal, 2).expect("fork");
        let timeline = TimeMachine::inspect(&fork).expect("inspect fork");

        // The prefix was kept, the tail was truncated.
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline.steps[0].name, "load");
        assert_eq!(timeline.steps[1].name, "decide");
        assert!(timeline.find("act").is_none(), "tail must be truncated");

        // The audit marker is in the log.
        let entries = fork.entries();
        let marker = entries.last().expect("marker entry");
        match &marker.kind {
            EntryKind::Marker { name, payload } => {
                assert_eq!(name, FORK_MARKER);
                assert_eq!(payload["kept_steps"], json!(2));
                assert_eq!(payload["source_steps"], json!(3));
            }
            other => panic!("expected fork marker, got {other:?}"),
        }

        // The original timeline did not change.
        assert_eq!(
            TimeMachine::inspect(&journal)
                .expect("inspect original")
                .len(),
            3
        );
    }

    #[test]
    fn fork_beyond_timeline_fails_closed() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let err = TimeMachine::fork(&journal, 4).expect_err("must fail");
        assert!(matches!(err, DurableError::InvalidFork { .. }));
    }

    #[test]
    fn fork_into_nonempty_target_fails_closed() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let target = InMemoryJournal::new();
        target
            .append(JournalEntry::completed(StepId::ZERO, "stale", json!(1)))
            .expect("append");
        let err = TimeMachine::fork_into(&journal, 1, &target).expect_err("must fail");
        assert!(matches!(err, DurableError::InvalidFork { .. }));
    }

    #[test]
    fn fork_at_zero_yields_empty_timeline_with_marker() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let fork = TimeMachine::fork(&journal, 0).expect("fork");
        let timeline = TimeMachine::inspect(&fork).expect("inspect");
        assert!(timeline.is_empty());
        assert_eq!(timeline.marker_count, 1, "audit marker present");
    }

    /// Core test: a forked continuation runs a counterfactual — the prefix
    /// replays from the log without side effects, the new continuation runs
    /// exactly once, and the original timeline does not change.
    #[test]
    fn forked_continuation_is_counterfactual_and_leaves_original_untouched() {
        let original_effects = Cell::new(0u32);
        let journal = three_step_run(&original_effects);
        assert_eq!(original_effects.get(), 1, "the original act ran once");
        let original_len = journal.len().expect("len");

        // Fork before the "decide" step and run a corrected policy.
        let fork = TimeMachine::fork(&journal, 1).expect("fork");
        let mut alt = DurableContext::new(fork).expect("alt ctx");

        let replay_effects = Cell::new(0u32);
        let amount: i64 = alt
            .step("load", || {
                replay_effects.set(replay_effects.get() + 1);
                Ok(0)
            })
            .expect("load replay");
        assert_eq!(amount, 100, "the prefix is restored from the log");
        assert_eq!(replay_effects.get(), 0, "replay does not run the closure");

        // Counterfactual: new decide policy + dry-run act.
        let recorder = DryRunRecorder::new();
        let approved: i64 = alt.step("decide", || Ok(amount / 2)).expect("decide alt");
        assert_eq!(approved, 50);
        let _receipt: String = alt
            .step("act", || {
                recorder.record("act", json!({"would_send": approved}));
                Ok(format!("dry:{approved}"))
            })
            .expect("act alt");

        // The intent was captured — nothing was dispatched (the type has no path).
        assert_eq!(recorder.len(), 1);
        assert_eq!(recorder.intents()[0].payload, json!({"would_send": 50}));

        // The original timeline is exactly unchanged.
        assert_eq!(journal.len().expect("len"), original_len);
        assert_eq!(original_effects.get(), 1, "no new real side effects");
    }

    #[test]
    fn fork_into_file_journal_replays_deterministically() {
        use crate::file::FileJournal;

        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-durable-tm-fork-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);

        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        // Fork into a persistent FileJournal.
        let target = FileJournal::open(&path).expect("open target");
        let kept = TimeMachine::fork_into(&journal, 2, &target).expect("fork_into");
        assert_eq!(kept, 2);
        drop(target);

        // "Restart": open the fork with a new handle and continue from there.
        let reopened = FileJournal::open(&path).expect("reopen");
        let mut ctx = DurableContext::new(reopened).expect("ctx");
        let a: i64 = ctx.step("load", || Ok(0)).expect("load");
        let b: i64 = ctx.step("decide", || Ok(0)).expect("decide");
        assert_eq!(
            (a, b),
            (100, 200),
            "the prefix is restored from the log even from disk"
        );
        assert!(!ctx.is_replaying());

        let _ = std::fs::remove_file(&path);
    }

    // ---------- Diff ----------

    #[test]
    fn diff_of_identical_timelines_is_identical() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let diff = TimeMachine::diff(&journal, &journal).expect("diff");
        assert!(diff.is_identical());
        assert_eq!(diff.changed_count(), 0);
        assert_eq!(diff.tail_count(), 0);
        assert_eq!(diff.first_divergence(), None);
        assert_eq!(diff.steps.len(), 3);
    }

    #[test]
    fn diff_reports_changed_steps_and_tails() {
        let effects = Cell::new(0u32);
        let original = three_step_run(&effects);

        // Fork: same load, different decide/act, plus an extra step.
        let fork = TimeMachine::fork(&original, 1).expect("fork");
        let mut alt = DurableContext::new(fork).expect("alt");
        let amount: i64 = alt.step("load", || Ok(0)).expect("load");
        let approved: i64 = alt.step("decide", || Ok(amount / 2)).expect("decide");
        let _r: String = alt
            .step("act", || Ok(format!("dry:{approved}")))
            .expect("act");
        let _extra: i64 = alt.step("audit", || Ok(1)).expect("audit");
        let forked = alt.finish();

        let diff = TimeMachine::diff(&original, &forked).expect("diff");
        assert!(!diff.is_identical());
        assert_eq!(diff.changed_count(), 2, "decide and act changed");
        assert_eq!(diff.tail_count(), 1, "audit only in the fork");
        assert_eq!(diff.first_divergence(), None, "names did not diverge");

        assert!(matches!(
            &diff.steps[0],
            StepDiff::Unchanged { name, .. } if name == "load"
        ));
        assert!(matches!(
            &diff.steps[1],
            StepDiff::Changed { name, .. } if name == "decide"
        ));
        assert!(matches!(
            &diff.steps[3],
            StepDiff::OnlyInAfter { name, .. } if name == "audit"
        ));
    }

    #[test]
    fn diff_detects_divergence_by_name() {
        let mut a = DurableContext::new(InMemoryJournal::new()).expect("a");
        let _ = a.step("x", || Ok::<_, String>(1)).expect("x");
        let a = a.finish();

        let mut b = DurableContext::new(InMemoryJournal::new()).expect("b");
        let _ = b.step("y", || Ok::<_, String>(1)).expect("y");
        let b = b.finish();

        let diff = TimeMachine::diff(&a, &b).expect("diff");
        assert_eq!(diff.first_divergence(), Some(0));
        assert!(matches!(
            &diff.steps[0],
            StepDiff::Diverged { before_name, after_name, .. }
                if before_name == "x" && after_name == "y"
        ));
    }

    #[test]
    fn diff_treats_failure_change_as_changed() {
        let mut a = DurableContext::new(InMemoryJournal::new()).expect("a");
        let _ = a.step("risky", || Ok::<_, String>(1)).expect("ok run");
        let a = a.finish();

        let mut b = DurableContext::new(InMemoryJournal::new()).expect("b");
        let _ = b.step::<i32, _>("risky", || Err("nope".to_string()));
        let b = b.finish();

        let diff = TimeMachine::diff(&a, &b).expect("diff");
        assert_eq!(diff.changed_count(), 1);
        match &diff.steps[0] {
            StepDiff::Changed { before, after, .. } => {
                assert!(before.is_completed());
                assert!(!after.is_completed());
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn diff_render_markdown_summarizes() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let text = TimeMachine::diff(&journal, &journal)
            .expect("diff")
            .render_markdown();
        assert!(text.contains("identical: true"));
        assert!(text.contains("unchanged"));
    }

    #[test]
    fn diff_serializes_to_json() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let diff = TimeMachine::diff(&journal, &journal).expect("diff");
        let json = serde_json::to_string(&diff).expect("serialize");
        assert!(json.contains("\"kind\":\"unchanged\""));
    }

    // ---------- DryRunRecorder ----------

    #[test]
    fn dry_run_recorder_captures_in_order() {
        let recorder = DryRunRecorder::new();
        assert!(recorder.is_empty());
        recorder.record("first", json!({"n": 1}));
        recorder.record("second", json!({"n": 2}));
        assert_eq!(recorder.len(), 2);
        let intents = recorder.intents();
        assert_eq!(intents[0].step, "first");
        assert_eq!(intents[1].step, "second");
        assert_eq!(intents[1].payload, json!({"n": 2}));
    }
}

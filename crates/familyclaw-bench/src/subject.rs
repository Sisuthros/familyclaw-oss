//! The [`Subject`] interface: *what* is being benchmarked.
//!
//! [`Subject`] is the harness's **seam** (design §2.1): FamilyClaw is the
//! first implementation, and competitors (Letta, OpenClaw, Hermes Agent)
//! come in behind the same interface as their own implementations without
//! requiring a harness redesign. A subject runs the continuity workload as
//! a black box: start a task → kill at a crash point → restart → recall
//! memory → sleep.
//!
//! ## Reproducibility
//! Every operation that needs time takes a [`Timestamp`] parameter —
//! **the system clock is never read**. The same input produces an
//! identical result on every run.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use familyclaw_core::Timestamp;

use crate::error::Result;

/// A single task in the continuity workload that a [`Subject`] executes.
///
/// A task is a deterministic script: the same `id` + `steps` → the same run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// The task's stable identifier (scenario-specific, deterministic).
    pub id: String,
    /// A human-readable description of what the task does.
    pub description: String,
    /// The steps to execute, in order (a deterministic script).
    pub steps: Vec<String>,
}

impl Task {
    /// Builds a task from an ID, description, and steps.
    #[must_use]
    pub fn new(id: impl Into<String>, description: impl Into<String>, steps: Vec<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            steps,
        }
    }
}

/// A handle to a running task run — [`Subject::kill`] targets this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHandle {
    /// The task ID this handle tracks.
    pub task_id: String,
    /// A subject-specific opaque reference (e.g. a journal path or PID).
    pub token: String,
}

impl RunHandle {
    /// Builds a handle from a task ID and subject token.
    #[must_use]
    pub fn new(task_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            token: token.into(),
        }
    }
}

/// Which point in the lifecycle a crash is forced at ([`Subject::kill`]).
///
/// These are the red-team attack points (design §3 S1, §5). `#[non_exhaustive]`
/// so new crash points can be added without breaking implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CrashPoint {
    /// Crash before the journal write (the step never made it to disk).
    BeforeWrite,
    /// Crash mid-journal-write — the last line is torn (cut off).
    MidWrite,
    /// Crash mid-replay (replay is interrupted and resumed again).
    MidReplay,
    /// The journal is corrupted (a non-final line was corrupted).
    CorruptedJournal,
    /// A clean stop — no crash, the comparison baseline.
    Clean,
}

/// A report on a restart ([`Subject::restart`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartReport {
    /// The number of steps recovered from the log.
    pub steps_replayed: usize,
    /// Whether the subject was in replay mode after the restart.
    pub was_replaying: bool,
    /// Whether side effects were re-executed during replay (target: 0).
    pub side_effects_reexecuted: usize,
    /// Whether the same end state was reached as the crash-free baseline.
    pub resumed_clean: bool,
}

/// A single memory-recall hit ([`Subject::recall`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    /// The content of the returned memory.
    pub content: String,
    /// The relevance score (`0.0..=1.0`).
    pub relevance: f32,
}

impl RecallHit {
    /// Builds a hit from content and relevance.
    #[must_use]
    pub fn new(content: impl Into<String>, relevance: f32) -> Self {
        Self {
            content: content.into(),
            relevance,
        }
    }
}

/// A summary of a sleep cycle ([`Subject::sleep_cycle`]).
///
/// Mirrors [`familyclaw_dream::DreamReport`] at the harness level without
/// depending on the subject's internal implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamSummary {
    /// The number of memories scanned.
    pub scanned: usize,
    /// The number of memories merged (deduplicated).
    pub merged: usize,
    /// The number of memories dropped (conflicting).
    pub dropped: usize,
    /// The number of dates absolutized.
    pub dates_absolutized: usize,
    /// The number of memories strengthened.
    pub strengthened: usize,
    /// The number of memories archived.
    pub archived: usize,
    /// Whether the protected identity anchors remained intact.
    pub protected_core_intact: bool,
}

/// The system under benchmark — a black box for the continuity workload.
///
/// FamilyClaw is the first implementation; competitors come in behind the
/// same interface (design §2.1). Every operation that needs time receives
/// a [`Timestamp`] injected — the system clock is never read.
#[async_trait]
pub trait Subject: Send {
    /// Starts a task and returns a handle for targeting a crash.
    ///
    /// # Errors
    /// Returns [`BenchError::Subject`](crate::BenchError::Subject) if
    /// starting the task fails.
    async fn start_task(&mut self, task: &Task, clock: Timestamp) -> Result<RunHandle>;

    /// Kills a running run at the given crash point.
    ///
    /// # Errors
    /// Returns an error if injecting the crash fails.
    async fn kill(&mut self, handle: &RunHandle, point: CrashPoint) -> Result<()>;

    /// Restarts the subject and reports the replay outcome.
    ///
    /// # Errors
    /// Returns an error if the restart or replay fails.
    async fn restart(&mut self, clock: Timestamp) -> Result<RestartReport>;

    /// Returns the memories matching a query in relevance order.
    ///
    /// # Errors
    /// Returns an error if the memory lookup fails.
    async fn recall(&mut self, query: &str, clock: Timestamp) -> Result<Vec<RecallHit>>;

    /// Runs a single sleep cycle (memory consolidation) and summarizes the result.
    ///
    /// # Errors
    /// Returns an error if the sleep cycle fails.
    async fn sleep_cycle(&mut self, clock: Timestamp) -> Result<DreamSummary>;

    /// The subject's stable name for the scorecard (e.g. `"familyclaw"`).
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_point_serializes_snake_case() {
        let json = serde_json::to_string(&CrashPoint::MidWrite).expect("serialize");
        assert_eq!(json, "\"mid_write\"");
    }

    #[test]
    fn task_and_handle_roundtrip() {
        let task = Task::new("t1", "demo", vec!["a".into(), "b".into()]);
        let json = serde_json::to_string(&task).expect("ser");
        let back: Task = serde_json::from_str(&json).expect("de");
        assert_eq!(task, back);

        let handle = RunHandle::new("t1", "tok");
        assert_eq!(handle.task_id, "t1");
    }
}

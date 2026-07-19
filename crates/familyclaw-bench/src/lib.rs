//! # familyclaw-bench
//!
//! **Continuity benchmark harness** — reproducible proof of the FamilyClaw
//! platform's continuity (design 2026-06-05, §2). This crate proves a
//! claim that competitors cannot refute:
//!
//! > *Kill a FamilyClaw agent mid-task. Restart it. It resumes from
//! > exactly the right step, every side effect runs exactly once, it
//! > remembers everything — and overnight its memory was cleaned up.*
//!
//! ## Architecture (seams)
//! - [`Subject`] — *what* is benchmarked. FamilyClaw now, competitors
//!   behind the same interface later (design §2.1). Runnable as a black box.
//! - [`Scenario`] — a scripted continuity workload (S1 Crash Matrix, S2
//!   Retention Curve, S3 Dream Quality).
//! - [`Harness`] — runs `Scenario × Subject → ScenarioResult` and
//!   assembles a [`Scorecard`].
//! - [`metrics`] — typed metrics (`resume_correctness`, `recall_at_k`,
//!   `dedup_precision`, `protected_core_intact`).
//! - [`Scorecard`] — a public artifact (JSON + markdown).
//!
//! ## Reproducibility (a hard requirement, design §2.2)
//! The wall clock is **injected** as a [`Timestamp`](familyclaw_core::Timestamp)
//! parameter everywhere — the system clock is never read. Same input →
//! identical scorecard on every run.
//!
//! ## OSS boundary (Layer A)
//! This crate is generic benchmark code. It does not hardcode family
//! members' souls, keys, tokens, IP addresses, or personal paths — all
//! subject-specific paths are supplied at runtime.

// Product names (FamilyClaw, OpenClaw, Letta, Hermes) appear in the docs as
// prose — they are not code symbols, so the doc_markdown backtick requirement
// does not apply to them.
#![allow(clippy::doc_markdown)]

pub mod comparative;
pub mod error;
pub mod harness;
pub mod metrics;
pub mod scenario;
pub mod scenarios;
pub mod scorecard;
pub mod security;
pub mod subject;
pub mod subjects;

pub use comparative::ComparativeScorecard;
pub use error::{BenchError, Result};
pub use harness::Harness;
pub use scenario::{Scenario, ScenarioResult};
pub use scorecard::Scorecard;
pub use security::{run_security_suite, to_security_markdown};
pub use subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task};
pub use subjects::{FamilyClawSubject, MarkdownFileSubject};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test will fail to compile.
        let task = Task::new("t", "d", Vec::new());
        assert_eq!(task.id, "t");
        let handle = RunHandle::new("t", "tok");
        assert_eq!(handle.token, "tok");
        let point = CrashPoint::Clean;
        assert_eq!(point, CrashPoint::Clean);
        let harness = Harness::new();
        // Harness is Copy — merely constructing it is enough to prove the seam.
        let _ = harness;
        let err: BenchError = BenchError::subject("x");
        assert!(matches!(err, BenchError::Subject(_)));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }
}

//! # familyclaw-durable
//!
//! **Durable substrate — deterministic replay (crash-proof).**
//!
//! This crate is layer 1 of the `FamilyClaw` platform (design §2.1) and the
//! **structural solution to a family's pain point #1 — memory
//! discontinuity**. Rather than relying on the agent to remember to save
//! its state, durable execution turns continuity into *structure*: the
//! workflow is recorded to an event journal, and if the process crashes,
//! the work resumes exactly where it left off — without replaying side
//! effects.
//!
//! ## Model
//! The implementation is **journal-based deterministic replay** (the
//! Temporal/Flawless model in pure Rust, without wasmtime at this stage):
//!
//! 1. The workflow is wrapped into steps via [`DurableContext::step`].
//! 2. Every completed step is written as a [`JournalEntry`] to an
//!    append-only [`Journal`].
//! 3. On restart, [`DurableContext`] is rebuilt from the same journal, and
//!    steps that already ran **are restored from the log without
//!    re-running their closures**.
//!
//! ## Example
//! ```
//! use familyclaw_durable::{DurableContext, InMemoryJournal};
//!
//! # fn main() -> familyclaw_durable::Result<()> {
//! // Fresh run: the closure executes and the result is written to the log.
//! let mut ctx = DurableContext::new(InMemoryJournal::new())?;
//! let greeting: String = ctx.step("greet", || Ok("hello".to_string()))?;
//! assert_eq!(greeting, "hello");
//!
//! // "Crash": take the journal and rebuild the context from it.
//! let journal = ctx.finish();
//! let mut resumed = DurableContext::new(journal)?;
//!
//! // Replay: the same step is restored from the log — the closure is NOT re-run.
//! let again: String = resumed.step("greet", || Ok("DIFFERENT".to_string()))?;
//! assert_eq!(again, "hello"); // the recorded value, not the closure's new value
//! # Ok(())
//! # }
//! ```
//!
//! ## Implementations
//! - [`InMemoryJournal`] — non-durable, for testing/development.
//! - [`FileJournal`] — crash-safe append-only JSONL (`flush` + `fsync`).
//! - [`PostgresJournal`] (feature `postgres`) — same journal contract on `PostgreSQL`.
//!
//! ## OSS boundary (Layer A)
//! This crate is generic platform code: it does not hardcode family
//! members' souls, keys, tokens, IP addresses, or personal paths. The
//! journal path is supplied at runtime.

pub mod context;
pub mod entry;
pub mod error;
pub mod file;
pub mod journal;
pub mod memory;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod time_machine;

pub use context::DurableContext;
pub use entry::{EntryKind, JournalEntry, StepId};
pub use error::{DurableError, Result};
pub use file::FileJournal;
pub use journal::Journal;
pub use memory::InMemoryJournal;
#[cfg(feature = "postgres")]
pub use postgres::PostgresJournal;
pub use time_machine::{
    DryRunRecorder, RecordedIntent, StepDiff, StepOutcome, TimeMachine, Timeline, TimelineDiff,
    TimelineStep, FORK_MARKER,
};

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
        // If any re-export is removed, this test stops compiling.
        let mut ctx: DurableContext<InMemoryJournal> =
            DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let v: u8 = ctx.step("s", || Ok(7)).expect("step");
        assert_eq!(v, 7);

        let entry: JournalEntry = JournalEntry::completed(StepId::ZERO, "s", serde_json::json!(7));
        let kind: &EntryKind = &entry.kind;
        assert!(!kind.is_snapshot());
        let err: DurableError = DurableError::step_failed("s", "boom");
        assert!(matches!(err, DurableError::StepFailed { .. }));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }

    /// End-to-end: two journal implementations produce the same deterministic
    /// replay result (confirms that `DurableContext` is genuinely journal-
    /// agnostic).
    #[test]
    fn in_memory_and_file_produce_identical_replay() {
        use std::cell::Cell;

        // Helper: run the workflow and return (result, side-effect count).
        fn run<J: Journal>(journal: J, effects: &Cell<u32>) -> (i64, J) {
            let mut ctx = DurableContext::new(journal).expect("ctx");
            let a: i64 = ctx
                .step("a", || {
                    effects.set(effects.get() + 1);
                    Ok(100)
                })
                .expect("a");
            let b: i64 = ctx
                .step("b", || {
                    effects.set(effects.get() + 1);
                    Ok(a + 23)
                })
                .expect("b");
            (b, ctx.finish())
        }

        // --- InMemory ---
        let mem_effects = Cell::new(0u32);
        let (mem_first, mem_journal) = run(InMemoryJournal::new(), &mem_effects);
        let (mem_replay, _) = run(mem_journal, &mem_effects);
        assert_eq!(mem_first, 123);
        assert_eq!(mem_replay, 123);
        assert_eq!(
            mem_effects.get(),
            2,
            "memory: no new side effects during replay"
        );

        // --- File ---
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-durable-e2e-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);

        let file_effects = Cell::new(0u32);
        let first_journal = FileJournal::open(&path).expect("open 1");
        let (file_first, _) = run(first_journal, &file_effects);
        // New handle = simulated restart.
        let second_journal = FileJournal::open(&path).expect("open 2");
        let (file_replay, _) = run(second_journal, &file_effects);

        assert_eq!(file_first, 123);
        assert_eq!(file_replay, 123);
        assert_eq!(
            file_effects.get(),
            2,
            "file: no new side effects during replay"
        );

        // Same result on both backends.
        assert_eq!(mem_replay, file_replay);

        let _ = std::fs::remove_file(&path);
    }
}

//! Concrete [`Subject`](crate::Subject) implementations.
//!
//! - [`FamilyClawSubject`] — runs the `continuity_daemon` binary as a black
//!   box child process (design §2.1).
//! - [`MarkdownFileSubject`] — an honest competitor baseline that models the
//!   documented behavior of file-based `MEMORY.md` memory (pure in-process).
//!   Makes the benchmark a head-to-head comparison.
//!
//! Further competitor adapters (Letta, OpenClaw, Hermes Agent) come in
//! behind the same [`Subject`](crate::Subject) interface as new modules,
//! without requiring a harness redesign.

pub mod familyclaw;
pub mod markdown_file;

pub use familyclaw::FamilyClawSubject;
pub use markdown_file::{CompetitorProfile, MarkdownFileSubject};

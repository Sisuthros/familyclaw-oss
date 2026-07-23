//! Concrete [`Subject`](crate::Subject) implementations.
//!
//! - [`FamilyClawSubject`] — runs the `continuity_daemon` binary as a black
//!   box child process (design §2.1).
//! - [`MarkdownFileSubject`] — an honest competitor baseline that models the
//!   documented behavior of file-based `MEMORY.md` memory (pure in-process).
//!   Makes the benchmark a head-to-head comparison.
//!
//! Further competitor adapters: process harnesses under
//! `bench-competitors/{openclaw,hermes,langgraph}/`. In-process shaped
//! baselines remain [`MarkdownFileSubject`].

pub mod familyclaw;
pub mod markdown_file;

pub use familyclaw::FamilyClawSubject;
pub use markdown_file::{CompetitorProfile, MarkdownFileSubject};

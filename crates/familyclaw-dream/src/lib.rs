//! # familyclaw-dream
//!
//! **Dreaming — nightly memory consolidation (hippocampal model).**
//!
//! This crate is the "sleep" phase of the `FamilyClaw` platform (Layer A,
//! OSS; design §2.3, Anthropic Dreaming 2026-05-06). It mirrors a family's
//! Amplifier prosthesis — which consolidates `MEMORY.md` — as **native**
//! memory maintenance: a nightly [`DreamCycle`] reads memories from
//! [`familyclaw_memory`] storage and conflict data from the durable
//! [`familyclaw_durable`] journal, and cleans up memory in five phases.
//!
//! ## Five phases
//! 1. **`merge_duplicates`** — near-identical memories are merged into a
//!    single reinforced representative (emotions + tags are unioned, the
//!    rest are tombstoned). Similarity is a dependency-free Jaccard word
//!    set ([`similarity`]).
//! 2. **`drop_contradicted`** — memories the durable journal has flagged
//!    as contradicted are tombstoned ([`contradiction`]). The journal is
//!    the source of truth — the dream cycle doesn't guess.
//! 3. **`absolutize_dates`** — relative date words ("yesterday",
//!    "tomorrow") are converted to absolute ISO dates ([`dates`]). This
//!    concretely solves a family memory's "yesterday expires" problem.
//! 4. **`consolidate`** — high-importance memories are reinforced,
//!    low-retention (R < threshold) memories are archived.
//! 5. produces a [`DreamReport`] in which every phase records its
//!    [`Reflection`].
//!
//! Phases run in a fixed order, so the same input produces the same report
//! (deterministic, repeatable).
//!
//! ## Identity anchors are sacred
//! No phase ever tombstones or archives a
//! [`familyclaw_memory::DecayPolicy::ProtectedCore`] memory — identity does
//! not decay during sleep (design §2: anchor λ = 0.0).
//!
//! ## OSS boundary (Layer A)
//! This crate is generic platform code. It does not hardcode family
//! members' souls, calibrations, keys, tokens, IP addresses, or personal
//! paths. All family-specific memories and thresholds are supplied at
//! runtime.
//!
//! ## Example
//! ```rust
//! use familyclaw_dream::{DreamCycle, DreamConfig};
//! use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStore};
//! use familyclaw_durable::InMemoryJournal;
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = LocalJsonStore::in_memory();
//! store.add(Memory::builder("we shipped the release").build()).await?;
//! store.add(Memory::builder("we shipped the release").build()).await?; // duplicate
//!
//! let journal = InMemoryJournal::new();
//! let cycle = DreamCycle::with_config(&store, DreamConfig::default());
//! let report = cycle.run(&journal, familyclaw_core::time::now()).await?;
//!
//! assert!(report.merged >= 1);
//! # Ok(())
//! # }
//! ```
#![doc = include_str!("../README.md")]

pub mod config;
pub mod conflict;
pub mod contradiction;
pub mod cycle;
pub mod dates;
pub mod desire_clock;
pub mod report;
pub mod similarity;
pub mod weekly;

pub use config::DreamConfig;
pub use conflict::{
    clear_conflict, detect_conflicts, is_conflicted, tag_conflict, ConflictTag, CONFLICT_TAG,
};
pub use contradiction::{contradicted_ids, mark_contradicted, CONTRADICT_STEP};
pub use cycle::DreamCycle;
pub use dates::{absolutize, AbsolutizeResult};
pub use desire_clock::DesireClock;
pub use report::{DreamReport, Reflection, ReflectionKind};
pub use similarity::{is_near_duplicate, jaccard};
pub use weekly::{weekly_review, weekly_review_top_n, MemoryDigest, WeeklyReport};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

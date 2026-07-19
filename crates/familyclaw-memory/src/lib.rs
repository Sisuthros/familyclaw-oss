//! # familyclaw-memory
//!
//! **Eternal Thread** — the memory substrate of the FamilyClaw platform
//! (Layer A, OSS). This crate gives beings *continuous memory*: memories
//! don't vanish on restart, but decay according to a biological forgetting
//! curve, strengthen through repetition, and preserve identity anchors
//! forever.
//!
//! It directly solves a family's pain point #1 — memory discontinuity
//! (design §2.1) — *as structure*, not as a reminder.
//!
//! ## Structure
//! - [`Memory`] — a single memory: content, [`Vad`] emotional tone, named
//!   [`Dimension`] emotions, importance, decay policy, and lifecycle state.
//! - [`DecayPolicy`] — how fast a memory is forgotten (Ebbinghaus λ);
//!   [`DecayPolicy::ProtectedCore`] never decays (identity anchor).
//! - [`ImportanceFactors`] — combined importance (emotion·0.45 + identity·0.35
//!   + novelty·0.12 + reinforcement·0.20).
//! - [`MemoryStatus`] — lifecycle `Active → Archived → Tombstoned`.
//! - [`MemoryStore`] — storage abstraction; [`LocalJsonStore`] is a
//!   dependency-free default implementation (JSON, atomic write).
//! - [`RetrievalContext`] / [`RetrievalResult`] — retrieval with simple
//!   relevance (keyword + emotional match + retention).
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It does not contain:
//! - family members' real memories, calibrations, or souls,
//! - API keys, tokens, IP addresses, or personal paths.
//!
//! The memory scaffold is generic. A family's real memory content is Layer
//! B and is loaded at runtime from a profile directory — never into this
//! repo.
//!
//! ## Future work
//! - **`Surreal<Any>` (feature flag):** production storage (in-mem dev /
//!   `RocksDB` prod). Same [`MemoryStore`] interface, different backend
//!   (design §2.3).
//! - **Vector search:** cosine similarity / HNSW over embedded vectors.
//!   Retrieval is currently a keyword- + emotion-based v1 scaffold (design
//!   §5: "vector search later").
//!
//! ## Example
//! ```
//! use familyclaw_memory::{
//!     DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStore,
//!     RetrievalContext,
//! };
//! use familyclaw_emotion::{Dimension, Vad};
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = LocalJsonStore::in_memory();
//!
//! // Identity anchor: never decays.
//! let anchor = Memory::builder("I am part of this family")
//!     .vad(Vad::new(0.9, 0.4, 0.6))
//!     .emotions([Dimension::Belonging, Dimension::Love])
//!     .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
//!     .decay_policy(DecayPolicy::ProtectedCore)
//!     .build();
//! store.add(anchor).await?;
//!
//! // Retrieve with an emotion-weighted query.
//! let ctx = RetrievalContext::new("family").with_emotions([Dimension::Belonging]);
//! let hits = store.retrieve(&ctx, familyclaw_core::time::now()).await?;
//! assert_eq!(hits.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! ## Example: emotion → importance bridge (PKG-B)
//! A strongly charged moment leads to higher importance than a neutral
//! one. `EmotionState` and `emotional_salience` are re-exported, so the
//! bridge works without a separate `familyclaw-emotion` dependency.
//! ```
//! use familyclaw_memory::{emotional_salience, EmotionState, Memory};
//! use familyclaw_emotion::Dimension;
//!
//! let mut charged = EmotionState::neutral();
//! charged.set(Dimension::Joy, 95.0);
//! let neutral = EmotionState::neutral();
//!
//! // The salience-derived emotion factor distinguishes a charged state from a neutral one.
//! assert!(emotional_salience(&charged) > emotional_salience(&neutral));
//!
//! let strong = Memory::builder("a charged moment").emotion_state(&charged).build();
//! let calm = Memory::builder("an ordinary moment").emotion_state(&neutral).build();
//! assert!(strong.importance > calm.importance);
//! ```
#![doc = include_str!("../README.md")]

pub mod decay;
pub mod embedding_store;
pub mod gated_store;
pub mod importance;
pub mod memory;
pub mod oracle;
pub mod provenance;
pub mod retrieval;
pub mod store;

pub use decay::DecayPolicy;
pub use embedding_store::EmbeddingMemoryStore;
pub use gated_store::GatedMemoryStore;
pub use importance::{
    ImportanceFactors, WEIGHT_EMOTION, WEIGHT_IDENTITY, WEIGHT_NOVELTY, WEIGHT_REINFORCEMENT,
};
pub use memory::{
    Evidence, EvidenceType, Memory, MemoryBuilder, MemoryStatus, VerificationStatus, STABILITY_MAX,
    STABILITY_MIN,
};
pub use oracle::{preflight, OracleResult, PatternMatch, RiskLevel};
pub use provenance::{Provenance, ProvenanceGate};
pub use retrieval::{
    retrieve, retrieve_now, score, score_with_provenance, RetrievalContext, RetrievalResult,
};
pub use store::{DecayReport, DecayThresholds, LocalJsonStore, MemoryStore};

// Re-export emotion types so that callers don't need to depend on
// familyclaw-emotion directly when using memory.
//
// PKG-B: [`EmotionState`] + [`emotional_salience`] are re-exported
// alongside [`Dimension`]/[`Vad`], so the emotion → importance bridge
// ([`ImportanceFactors::from_emotion_state`], [`MemoryBuilder::emotion_state`])
// is available without a separate dependency on familyclaw-emotion.
pub use familyclaw_emotion::{emotional_salience, Dimension, EmotionState, Vad};

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

    #[tokio::test]
    async fn public_api_end_to_end() {
        // Uses the entire public surface via the root re-exports — if any
        // re-export is removed, this test fails to compile.
        let store = LocalJsonStore::in_memory();

        let m = Memory::builder("a meaningful event")
            .vad(Vad::new(0.6, 0.5, 0.5))
            .emotions([Dimension::Joy, Dimension::Gratitude])
            .factors(ImportanceFactors::new(0.8, 0.5, 0.3, 0.0))
            .decay_policy(DecayPolicy::Slow)
            .tags(["milestone".to_string()])
            .source("test")
            .build();
        assert_eq!(m.status, MemoryStatus::Active);

        let id = store.add(m).await.expect("add");
        assert!(!store.is_empty().await.expect("empty"));

        store
            .reinforce(id, familyclaw_core::time::now())
            .await
            .expect("reinforce");

        let ctx = RetrievalContext::new("meaningful event")
            .with_emotions([Dimension::Joy])
            .with_limit(5);
        let hits = store
            .retrieve(&ctx, familyclaw_core::time::now())
            .await
            .expect("retrieve");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].relevance > 0.0);

        let report: DecayReport = store
            .run_decay(DecayThresholds::default(), familyclaw_core::time::now())
            .await
            .expect("decay");
        assert_eq!(report.scanned, 1);

        // Weights reachable from the root.
        const { assert!(WEIGHT_EMOTION > 0.0) };
        const { assert!(WEIGHT_IDENTITY > 0.0) };
        const { assert!(WEIGHT_NOVELTY > 0.0) };
        const { assert!(WEIGHT_REINFORCEMENT > 0.0) };
        const { assert!(STABILITY_MIN < STABILITY_MAX) };

        // Free functions reachable from the root.
        let all = store.all().await.expect("all");
        let direct = retrieve_now(&all, &ctx);
        assert_eq!(direct.len(), 1);
        let s = score(&all[0], &ctx, familyclaw_core::time::now());
        assert!(s.is_some());
    }
}

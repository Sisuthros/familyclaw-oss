//! S6 Embedding Recall — vector-based retrieval with a real embedding provider.
//!
//! This is the roadmap's **D4 recall-benchmark-gate**: before vector search
//! (`semantic_weight > 0` + embeddings) is taken seriously, it has to **prove
//! it beats keyword search** on a fixed fixture. Unlike S5 (which tests
//! `RetrievalContext`'s built-in substring semantics), this exercises the REAL
//! vector path:
//!
//! 1. memories are stored via [`EmbeddingMemoryStore`], which populates the
//!    `embedding` field using [`DeterministicEmbedder`];
//! 2. the query is embedded with the SAME provider and supplied via
//!    [`RetrievalContext::with_query_embedding`];
//! 3. retrieval compares the query and memory vectors by cosine similarity.
//!
//! ## Fixture
//! The default provider is a feature-hashing bag-of-words → it rewards
//! **shared words**. The fixture is built so the correct memory shares
//! vocabulary with the query while the distractor does not:
//! - correct:    "the deployment pipeline shipped the release build"
//! - distractor: "ocean waves crash on the quiet midnight shore"
//!
//! The query "deployment pipeline release build" shares four content words
//! with the correct memory and zero with the distractor → vector cosine
//! separates the correct one. **Gate:** vector-space separation
//! `cos(query,correct) - cos(query,distractor) > 0` AND the correct memory is
//! top-1 AND the distractor is not top-1. (Separation is a more honest metric
//! than an absolute keyword-vs-vector comparison, which would be unfair when
//! the same words are shared.)

use std::sync::Arc;

use async_trait::async_trait;
use familyclaw_core::Timestamp;
use familyclaw_embeddings::{DeterministicEmbedder, EmbeddingProvider};
use familyclaw_memory::{
    EmbeddingMemoryStore, ImportanceFactors, LocalJsonStore, Memory, MemoryStore, RetrievalContext,
};

use crate::error::Result;
use crate::scenario::{Scenario, ScenarioResult};
use crate::subject::Subject;

/// S6 Embedding Recall — vector search with a real embedding provider (D4 gate).
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddingRecall;

impl EmbeddingRecall {
    /// The scenario's unique identifier.
    pub const ID: &'static str = "s6_embedding_recall";

    /// Creates a new EmbeddingRecall scenario.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

const MEM_CORRECT: &str = "the deployment pipeline shipped the release build";
const MEM_DISTRACTOR: &str = "ocean waves crash on the quiet midnight shore";
const QUERY: &str = "deployment pipeline release build";

#[async_trait]
impl Scenario for EmbeddingRecall {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(&self, _subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult> {
        let embedder = DeterministicEmbedder::new();
        // Auto-embedding memory store: stored memories get their embedding on write.
        let store = EmbeddingMemoryStore::new(
            LocalJsonStore::in_memory(),
            Arc::new(DeterministicEmbedder::new()),
        );

        let id_correct = store
            .add(
                Memory::builder(MEM_CORRECT)
                    .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
                    .created_at(clock)
                    .build(),
            )
            .await?;
        let id_distractor = store
            .add(
                Memory::builder(MEM_DISTRACTOR)
                    .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
                    .created_at(clock)
                    .build(),
            )
            .await?;

        // Keyword search (semantic_weight 0.0): no vector involved.
        let ctx_kw = RetrievalContext::new(QUERY)
            .with_limit(2)
            .with_semantic_weight(0.0);
        let hits_kw = store.retrieve(&ctx_kw, clock).await?;

        // Vector search: the query is embedded with the SAME provider + weight > 0.
        let query_vec = embedder.embed(QUERY);
        let ctx_vec = RetrievalContext::new(QUERY)
            .with_limit(2)
            .with_semantic_weight(0.7)
            .with_query_embedding(query_vec);
        let hits_vec = store.retrieve(&ctx_vec, clock).await?;

        // Vector-space SEPARATION: cosine(query, correct) vs cosine(query,
        // distractor). This is the heart of the D4 gate — it proves that
        // embeddings separate the relevant from the irrelevant, independent of
        // keyword scores (which would be an unfair comparison given the shared
        // words). Computed directly from the stored embeddings.
        let q = embedder.embed(QUERY);
        let correct_emb = store
            .get(id_correct)
            .await?
            .and_then(|m| m.embedding)
            .unwrap_or_default();
        let distractor_emb = store
            .get(id_distractor)
            .await?
            .and_then(|m| m.embedding)
            .unwrap_or_default();
        let cos_correct = cosine(&q, &correct_emb);
        let cos_distractor = cosine(&q, &distractor_emb);
        let separation = cos_correct - cos_distractor;

        let top1_correct = hits_vec.first().is_some_and(|h| h.memory.id == id_correct);
        let distractor_is_top1 = hits_vec
            .first()
            .is_some_and(|h| h.memory.id == id_distractor);

        // Gate: vector space separates correct from distractor (separation > 0)
        // AND retrieval ranks correct as top-1 AND distractor is not top-1.
        let passed = separation > 0.0 && top1_correct && !distractor_is_top1;

        let kw_top1_correct = hits_kw.first().is_some_and(|h| h.memory.id == id_correct);

        let result = ScenarioResult::new(Self::ID, passed)
            .with_metric("vector_separation", f64::from(separation))
            .with_metric("cos_correct", f64::from(cos_correct))
            .with_metric("cos_distractor", f64::from(cos_distractor))
            .with_metric("top1_is_correct", if top1_correct { 1.0 } else { 0.0 })
            .with_metric(
                "keyword_top1_is_correct",
                if kw_top1_correct { 1.0 } else { 0.0 },
            )
            .with_metric(
                "embedder_dim",
                f64::from(u32::try_from(embedder.dimensions()).unwrap_or(u32::MAX)),
            )
            .with_note(format!(
                "provider={}, cos(query,correct)={cos_correct:.3}, cos(query,distractor)={cos_distractor:.3}, separation={separation:.3}",
                embedder.id()
            ));
        Ok(result)
    }
}

/// Cosine similarity between two vectors of equal length. Returns 0.0 if the
/// lengths differ, a vector is empty, or either norm is zero. (Local copy —
/// bench does not depend on `familyclaw-memory`'s internal cosine.)
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::{CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Task};

    struct StubSubject;

    #[async_trait]
    impl Subject for StubSubject {
        async fn start_task(&mut self, task: &Task, _c: Timestamp) -> Result<RunHandle> {
            Ok(RunHandle::new(task.id.clone(), "stub"))
        }
        async fn kill(&mut self, _h: &RunHandle, _p: CrashPoint) -> Result<()> {
            Ok(())
        }
        async fn restart(&mut self, _c: Timestamp) -> Result<RestartReport> {
            Ok(RestartReport {
                steps_replayed: 0,
                was_replaying: false,
                side_effects_reexecuted: 0,
                resumed_clean: true,
            })
        }
        async fn recall(&mut self, _q: &str, _c: Timestamp) -> Result<Vec<RecallHit>> {
            Ok(vec![RecallHit::new("memory", 0.5)])
        }
        async fn sleep_cycle(&mut self, _c: Timestamp) -> Result<DreamSummary> {
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
        fn name(&self) -> &'static str {
            "stub_s6"
        }
    }

    #[tokio::test]
    async fn embedding_recall_passes_with_deterministic_provider() {
        let scenario = EmbeddingRecall::new();
        let mut subject = StubSubject;
        let clock = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        let result = scenario.run(&mut subject, clock).await.expect("run");

        assert_eq!(result.id, EmbeddingRecall::ID);
        assert!(result.passed, "S6 should pass: {:?}", result.notes);
        let sep = result
            .metrics
            .get("vector_separation")
            .copied()
            .unwrap_or(0.0);
        assert!(sep > 0.0, "vector_separation must be > 0, got {sep}");
        assert_eq!(
            result.metrics.get("top1_is_correct").copied(),
            Some(1.0),
            "correct memory must be top-1 under vector search"
        );
    }
}

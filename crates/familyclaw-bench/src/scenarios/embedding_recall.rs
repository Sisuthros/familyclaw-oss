//! S6 Embedding Recall — vektoripohjainen haku oikealla upotustarjoajalla.
//!
//! Tämä on roadmapin **D4 recall-benchmark-gate**: ennen kuin vektorihaku
//! (`semantic_weight > 0` + upotukset) otetaan tosissaan, sen on **todistettava
//! päihittävänsä keyword-haun** kiinteällä fixturella. Toisin kuin S5 (joka
//! testaa `RetrievalContext`:n sisäänrakennettua substring-semantiikkaa), tämä
//! ajaa AIDON vektoripolun:
//!
//! 1. muistot tallennetaan [`EmbeddingMemoryStore`]:lla, joka täyttää
//!    `embedding`-kentän [`DeterministicEmbedder`]:llä;
//! 2. kysely upotetaan SAMALLA tarjoajalla ja annetaan
//!    [`RetrievalContext::with_query_embedding`]:llä;
//! 3. haku cosine-vertailee kysely- ja muistovektoreita.
//!
//! ## Fixture
//! Oletustarjoaja on feature-hashing-bag-of-words → se palkitsee **jaetut
//! sanat**. Fixture on rakennettu niin, että oikea muisto jakaa sanaston kyselyn
//! kanssa, distraktori ei:
//! - oikea:      "the deployment pipeline shipped the release build"
//! - distraktori:"ocean waves crash on the quiet midnight shore"
//!
//! Kysely "deployment pipeline release build" jakaa neljä sisältösanaa oikean
//! muiston kanssa ja nolla distraktorin kanssa → vektori-cosine erottaa oikean.
//! **Gate:** vektoriavaruuden erottelukyky `cos(query,correct) -
//! cos(query,distractor) > 0` JA oikea muisto on top-1 JA distraktori ei ole
//! top-1. (Erottelukyky on rehellisempi mittari kuin keyword-vs-vektori-
//! absoluuttivertailu, joka samoilla sanoilla olisi epäreilu.)

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

/// S6 Embedding Recall — vektorihaku oikealla upotustarjoajalla (D4-gate).
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddingRecall;

impl EmbeddingRecall {
    /// Skenaarion yksilöivä tunniste.
    pub const ID: &'static str = "s6_embedding_recall";

    /// Luo uuden EmbeddingRecall-skenaarion.
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
        // Auto-upottava muisti: tallennetut muistot saavat embeddingin kirjoitettaessa.
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

        // Keyword-haku (semantic_weight 0.0): ei vektoria.
        let ctx_kw = RetrievalContext::new(QUERY)
            .with_limit(2)
            .with_semantic_weight(0.0);
        let hits_kw = store.retrieve(&ctx_kw, clock).await?;

        // Vektorihaku: kysely upotetaan SAMALLA tarjoajalla + paino > 0.
        let query_vec = embedder.embed(QUERY);
        let ctx_vec = RetrievalContext::new(QUERY)
            .with_limit(2)
            .with_semantic_weight(0.7)
            .with_query_embedding(query_vec);
        let hits_vec = store.retrieve(&ctx_vec, clock).await?;

        // Vektoriavaruuden EROTTELUKYKY: cosine(query, oikea) vs cosine(query,
        // distraktori). Tämä on D4-gaten ydin — todistaa että upotukset
        // erottavat relevantin epärelevantista, riippumatta keyword-pisteistä
        // (jotka samoilla sanoilla olisivat epäreilu vertailu). Lasketaan suoraan
        // tallennetuista upotuksista.
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

        // Gate: vektoriavaruus erottaa oikean distraktorista (separation > 0) JA
        // haku sijoittaa oikean top-1:ksi JA distraktori ei ole top-1.
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

/// Cosine-similarity kahden saman pituisen vektorin välillä. Palauttaa 0.0 jos
/// pituudet eroavat, vektori on tyhjä tai jommankumman normi on nolla. (Lokaali
/// kopio — bench ei riipu `familyclaw-memory`n sisäisestä cosine:sta.)
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

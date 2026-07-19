//! Automatic embedding on write ([`EmbeddingMemoryStore`]).
//!
//! This is a **wrapper** ([decorator]) around any [`MemoryStore`]
//! (such as [`GatedMemoryStore`](crate::GatedMemoryStore)). It fills the memory's
//! [`embedding`](crate::Memory::embedding) field
//! using an [`EmbeddingProvider`] **before** delegating to the inner storage, if
//! the field is still empty. This way vector search (cosine similarity
//! in [`crate::retrieval`]) gets embeddings automatically — the caller does not
//! need to embed manually.
//!
//! ## Why a wrapper, not built-in
//! Same reason as [`GatedMemoryStore`](crate::GatedMemoryStore): embedding is
//! an **optional, additive layer**. The default provider under the
//! zero-dependency constraint ([`DeterministicEmbedder`](familyclaw_embeddings::DeterministicEmbedder)) has
//! no dependencies, but heavier (feature-gated) providers must not be forced onto
//! every storage. The wrapper lets the caller choose: no wrapper = no
//! embedding (current keyword-based retrieval), wrapper = automatic embedding.
//!
//! ## Idempotence + existing embeddings
//! If a memory already has an `embedding`, it is NOT overwritten (the vector
//! provided by the caller or produced by a different provider is respected). A
//! zero vector resulting from empty content is left unset (a zero norm doesn't
//! help cosine similarity), so the stored data stays clean.
//!
//! [decorator]: https://en.wikipedia.org/wiki/Decorator_pattern

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use familyclaw_core::{MessageId, Result, Timestamp};
use familyclaw_embeddings::EmbeddingProvider;

use crate::memory::{Memory, MemoryStatus};
use crate::retrieval::{RetrievalContext, RetrievalResult};
use crate::store::{DecayReport, DecayThresholds, MemoryStore};

/// Type-erased future for a dyn-compatible trait (same shape as used in
/// [`MemoryStore`]'s methods). `'a` captures the `&self` borrow.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A [`MemoryStore`] wrapper that embeds memories automatically on write.
///
/// Delegates all operations to the inner storage `S`, but
/// [`add`](MemoryStore::add) and [`update`](MemoryStore::update) fill in the
/// missing [`embedding`](Memory::embedding) field before delegating.
pub struct EmbeddingMemoryStore<S> {
    inner: S,
    embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
}

impl<S> EmbeddingMemoryStore<S> {
    /// Wraps the inner storage with the given embedding provider.
    pub fn new(inner: S, embedder: Arc<dyn EmbeddingProvider + Send + Sync>) -> Self {
        Self { inner, embedder }
    }

    /// The wrapped inner storage (read access).
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Unwraps the wrapper and returns the inner storage.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// The provider's stable identifier (e.g. for `status`/`doctor` reporting).
    pub fn embedder_id(&self) -> &str {
        self.embedder.id()
    }

    /// Fills the memory's `embedding` field if it is empty and the content
    /// yields a non-zero vector. Returns the memory (possibly enriched).
    fn enrich(&self, mut memory: Memory) -> Memory {
        if memory.embedding.is_none() && !memory.content.trim().is_empty() {
            let vec = self.embedder.embed(&memory.content);
            // A zero vector (e.g. from separators only) is not stored: it
            // doesn't help cosine similarity and only bloats stored data.
            if vec.iter().any(|&x| x != 0.0) {
                memory.embedding = Some(vec);
            }
        }
        memory
    }
}

impl<S: MemoryStore> MemoryStore for EmbeddingMemoryStore<S> {
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>> {
        let enriched = self.enrich(memory);
        Box::pin(async move { self.inner.add(enriched).await })
    }

    fn get(&self, id: MessageId) -> BoxFuture<'_, Result<Option<Memory>>> {
        Box::pin(async move { self.inner.get(id).await })
    }

    fn update(&self, memory: Memory) -> BoxFuture<'_, Result<()>> {
        let enriched = self.enrich(memory);
        Box::pin(async move { self.inner.update(enriched).await })
    }

    fn reinforce(&self, id: MessageId, at: Timestamp) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.inner.reinforce(id, at).await })
    }

    fn set_status(&self, id: MessageId, status: MemoryStatus) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.inner.set_status(id, status).await })
    }

    fn all(&self) -> BoxFuture<'_, Result<Vec<Memory>>> {
        Box::pin(async move { self.inner.all().await })
    }

    fn len(&self) -> BoxFuture<'_, Result<usize>> {
        Box::pin(async move { self.inner.len().await })
    }

    fn is_empty(&self) -> BoxFuture<'_, Result<bool>> {
        Box::pin(async move { self.inner.is_empty().await })
    }

    fn retrieve(
        &self,
        ctx: &RetrievalContext,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<Vec<RetrievalResult>>> {
        // Retrieval is delegated as-is. (This wrapper only enriches writes;
        // the query embedding is set by the caller on `RetrievalContext`.)
        // `ctx` is cloned so the returned future does not borrow a short-lived reference.
        let ctx = ctx.clone();
        Box::pin(async move { self.inner.retrieve(&ctx, at).await })
    }

    fn run_decay(
        &self,
        thresholds: DecayThresholds,
        at: Timestamp,
    ) -> BoxFuture<'_, Result<DecayReport>> {
        Box::pin(async move { self.inner.run_decay(thresholds, at).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LocalJsonStore;
    use familyclaw_embeddings::DeterministicEmbedder;

    fn embedder() -> Arc<dyn EmbeddingProvider + Send + Sync> {
        Arc::new(DeterministicEmbedder::new())
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EmbeddingMemoryStore<LocalJsonStore>>();
    }

    #[tokio::test]
    async fn add_fills_missing_embedding() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        let id = store
            .add(Memory::builder("kissa istuu matolla").build())
            .await
            .expect("add");
        let stored = store.get(id).await.expect("get").expect("present");
        let emb = stored.embedding.expect("embedding filled");
        assert_eq!(emb.len(), DeterministicEmbedder::DEFAULT_DIMENSIONS);
        assert!(emb.iter().any(|&x| x != 0.0));
    }

    #[tokio::test]
    async fn add_does_not_overwrite_existing_embedding() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        let preset = vec![0.5f32; 4];
        let mem = Memory::builder("teksti").embedding(preset.clone()).build();
        let id = store.add(mem).await.expect("add");
        let stored = store.get(id).await.expect("get").expect("present");
        assert_eq!(
            stored.embedding,
            Some(preset),
            "existing embedding must not be overwritten"
        );
    }

    #[tokio::test]
    async fn empty_content_gets_no_embedding() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        // Separators only → zero vector → not stored.
        let id = store
            .add(Memory::builder("   ").build())
            .await
            .expect("add");
        let stored = store.get(id).await.expect("get").expect("present");
        assert!(stored.embedding.is_none(), "zero vector must not be stored");
    }

    #[tokio::test]
    async fn delegates_len_and_get_to_inner() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        assert_eq!(store.len().await.expect("len"), 0);
        store
            .add(Memory::builder("yksi").build())
            .await
            .expect("add");
        assert_eq!(store.len().await.expect("len"), 1);
        assert!(!store.is_empty().await.expect("is_empty"));
    }

    #[tokio::test]
    async fn embedder_id_is_exposed() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        assert_eq!(store.embedder_id(), "deterministic-hash-v1");
    }
}

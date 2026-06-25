//! Automaattinen upotus kirjoitettaessa ([`EmbeddingMemoryStore`]).
//!
//! Tämä on **kääre** ([decorator]) minkä tahansa [`MemoryStore`]:n ympärille
//! (kuten [`GatedMemoryStore`](crate::GatedMemoryStore)). Se täyttää muiston
//! [`embedding`](crate::Memory::embedding)-kentän
//! [`EmbeddingProvider`]:lla **ennen** delegointia sisempään tallennukseen, jos
//! kentät on vielä tyhjä. Näin vektorihaku (cosine-similarity
//! [`crate::retrieval`]ssä) saa upotukset automaattisesti — kutsujan ei tarvitse
//! upottaa käsin.
//!
//! ## Miksi kääre, ei sisäänrakennettu
//! Sama syy kuin [`GatedMemoryStore`](crate::GatedMemoryStore)lla: upotus on
//! **valinnainen, lisättävä kerros**. Köyhyys-rajoitteen oletustarjoaja
//! ([`DeterministicEmbedder`](familyclaw_embeddings::DeterministicEmbedder)) on
//! riippuvuudeton, mutta raskaammat (feature-gated) tarjoajat eivät saa pakottua
//! jokaiseen tallennukseen. Kääre antaa kutsujan valita: ei käärettä = ei
//! upotusta (nykyinen avainsanapohjainen haku), kääre = automaattinen upotus.
//!
//! ## Idempotenssi + olemassa olevat upotukset
//! Jos muistolla on jo `embedding`, sitä EI ylikirjoiteta (kunnioitetaan
//! kutsujan antamaa tai eri tarjoajan tuottamaa vektoria). Tyhjästä sisällöstä
//! tuleva nollavektori jätetään asettamatta (nollanormi ei auta cosinea), jotta
//! tallennettu data pysyy siistinä.
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

/// Tyyppi-pyyhitty future dyn-yhteensopivalle traitille (sama muoto kuin
/// [`MemoryStore`]:n metodeissa). `'a` vangitsee `&self`-lainan.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// [`MemoryStore`]-kääre joka upottaa muistot automaattisesti kirjoitettaessa.
///
/// Delegoi kaikki operaatiot sisempään tallennukseen `S`, mutta
/// [`add`](MemoryStore::add) ja [`update`](MemoryStore::update) täyttävät
/// puuttuvan [`embedding`](Memory::embedding)-kentän ennen delegointia.
pub struct EmbeddingMemoryStore<S> {
    inner: S,
    embedder: Arc<dyn EmbeddingProvider + Send + Sync>,
}

impl<S> EmbeddingMemoryStore<S> {
    /// Kääräisee sisemmän tallennuksen annetulla upotustarjoajalla.
    pub fn new(inner: S, embedder: Arc<dyn EmbeddingProvider + Send + Sync>) -> Self {
        Self { inner, embedder }
    }

    /// Kääritty sisempi tallennus (lukuoikeus).
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Purkaa kääreen ja palauttaa sisemmän tallennuksen.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Tarjoajan vakaa tunniste (esim. raportointiin `status`/`doctor`).
    pub fn embedder_id(&self) -> &str {
        self.embedder.id()
    }

    /// Täyttää muiston `embedding`-kentän jos se on tyhjä ja sisällöstä syntyy
    /// ei-nollavektori. Palauttaa muiston (mahdollisesti rikastettuna).
    fn enrich(&self, mut memory: Memory) -> Memory {
        if memory.embedding.is_none() && !memory.content.trim().is_empty() {
            let vec = self.embedder.embed(&memory.content);
            // Nollavektoria (esim. pelkkiä erottimia) ei tallenneta: se ei auta
            // cosinea ja vain paisuttaa tallennettua dataa.
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
        // Haku delegoidaan sellaisenaan. (Tämä kääre rikastaa vain kirjoitusta;
        // kysely-embeddingin asettaa kutsuja `RetrievalContext`:iin.) `ctx`
        // kloonataan, jotta palautettu future ei lainaa lyhytikäistä viittausta.
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
        let emb = stored.embedding.expect("embedding täytetty");
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
            "olemassa olevaa ei ylikirjoiteta"
        );
    }

    #[tokio::test]
    async fn empty_content_gets_no_embedding() {
        let store = EmbeddingMemoryStore::new(LocalJsonStore::in_memory(), embedder());
        // Pelkkiä erottimia → nollavektori → ei tallenneta.
        let id = store
            .add(Memory::builder("   ").build())
            .await
            .expect("add");
        let stored = store.get(id).await.expect("get").expect("present");
        assert!(stored.embedding.is_none(), "nollavektoria ei tallenneta");
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

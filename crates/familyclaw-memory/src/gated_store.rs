//! Provenance-gated memory storage ([`GatedMemoryStore`]).
//!
//! [`GatedMemoryStore`] wraps any [`MemoryStore`] implementation and
//! enforces the [`ProvenanceGate`] poisoning protection **at write time**:
//! before a memory enters the inner storage, its provenance is weighed by
//! the gate. A low-trust external source ([`Provenance::External`](crate::Provenance::External) whose
//! `trust` falls below the gate's threshold) is rejected, so it cannot
//! contaminate later retrieval (*Sleeper Memory Poisoning* protection, see
//! [`crate::provenance`]).
//!
//! Direct experience ([`Provenance::DirectExperience`](crate::Provenance::DirectExperience)) and derived memories
//! ([`Provenance::Derived`](crate::Provenance::Derived)) always pass through — only external claims
//! are weighed.
//!
//! ## Design
//! - **Additive:** does not change the [`MemoryStore`] trait or the
//!   [`LocalJsonStore`](crate::LocalJsonStore) implementation. The gate is a new, optional layer.
//! - **Transparent:** all other methods ([`get`](MemoryStore::get),
//!   [`retrieve`](MemoryStore::retrieve), [`run_decay`](MemoryStore::run_decay)
//!   etc.) are delegated as-is to the inner storage.
//! - **Write gate:** only [`add`](MemoryStore::add) and
//!   [`update`](MemoryStore::update) are weighed. Confirming or transitioning
//!   the lifecycle state of an existing memory does not introduce new
//!   provenance, so those are not re-weighed.
//!
//! ## Example
//! ```
//! use familyclaw_memory::{
//!     GatedMemoryStore, LocalJsonStore, Memory, MemoryStore, Provenance, ProvenanceGate,
//! };
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
//!
//! // Direct experience always passes.
//! let trusted = Memory::builder("I saw this myself")
//!     .provenance(Provenance::DirectExperience)
//!     .build();
//! store.add(trusted).await?;
//!
//! // A low-trust external claim is rejected.
//! let poisoned = Memory::builder("an untrusted claim")
//!     .provenance(Provenance::external("web", 0.1))
//!     .build();
//! assert!(store.add(poisoned).await.is_err());
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::pin::Pin;

use familyclaw_core::{FamilyClawError, MessageId, Result, Timestamp};

use crate::memory::{Memory, MemoryStatus};
use crate::provenance::ProvenanceGate;
use crate::retrieval::{RetrievalContext, RetrievalResult};
use crate::store::{DecayReport, DecayThresholds, MemoryStore};

/// Type-erased future, matching the shape used by the [`MemoryStore`]
/// trait's methods. The lifetime `'a` captures the `&self` borrow, so the
/// returned future can reference `self` (and thus the inner storage).
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A provenance-gated wrapper around a [`MemoryStore`] implementation.
///
/// Enforces the [`ProvenanceGate`] poisoning protection at write time and
/// delegates everything else to the inner storage. Created either with an
/// explicit gate ([`new`](GatedMemoryStore::new)) or the default gate
/// ([`with_default_gate`](GatedMemoryStore::with_default_gate)).
#[derive(Debug)]
pub struct GatedMemoryStore<S: MemoryStore> {
    /// The wrapped inner storage to which accepted writes are delegated.
    inner: S,
    /// The provenance gate that weighs the provenance of every write.
    gate: ProvenanceGate,
}

impl<S: MemoryStore> GatedMemoryStore<S> {
    /// Wraps `inner` storage with the given gate.
    #[must_use]
    pub fn new(inner: S, gate: ProvenanceGate) -> Self {
        Self { inner, gate }
    }

    /// Wraps `inner` storage with the default gate
    /// ([`ProvenanceGate::default`], threshold `0.5`).
    #[must_use]
    pub fn with_default_gate(inner: S) -> Self {
        Self {
            inner,
            gate: ProvenanceGate::default(),
        }
    }

    /// The gate that weighs the provenance of writes.
    #[must_use]
    pub const fn gate(&self) -> &ProvenanceGate {
        &self.gate
    }

    /// The wrapped inner storage (read access).
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Unwraps the wrapper and returns the inner storage.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Builds a rejection error when provenance fails to pass the gate.
    fn rejected(&self) -> FamilyClawError {
        FamilyClawError::invalid_input(format!(
            "provenance rejected: source trust below gate threshold {}",
            self.gate.min_trust()
        ))
    }
}

impl<S: MemoryStore> MemoryStore for GatedMemoryStore<S> {
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>> {
        // Weigh provenance BEFORE delegating: a low-trust external
        // source must not even attempt to write to the inner storage.
        if !self.gate.admit(&memory.provenance) {
            let err = self.rejected();
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move { self.inner.add(memory).await })
    }

    fn get(&self, id: MessageId) -> BoxFuture<'_, Result<Option<Memory>>> {
        Box::pin(async move { self.inner.get(id).await })
    }

    fn update(&self, memory: Memory) -> BoxFuture<'_, Result<()>> {
        // An update may introduce new provenance → weighed the same as add.
        if !self.gate.admit(&memory.provenance) {
            let err = self.rejected();
            return Box::pin(async move { Err(err) });
        }
        Box::pin(async move { self.inner.update(memory).await })
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
    // Some tests compare exact f32 constants (the gate threshold) — exact
    // comparison is intentional and safe here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::importance::ImportanceFactors;
    use crate::provenance::Provenance;
    use crate::store::LocalJsonStore;
    use familyclaw_core::time;

    fn mem_with(content: &str, provenance: Provenance) -> Memory {
        Memory::builder(content)
            .factors(ImportanceFactors::new(0.5, 0.0, 0.0, 0.0))
            .provenance(provenance)
            .build()
    }

    #[test]
    fn gated_store_is_send_sync() {
        // The wrapper must preserve Send + Sync, so it remains usable in
        // multithreaded tokio execution just like the inner storage.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GatedMemoryStore<LocalJsonStore>>();
    }

    #[tokio::test]
    async fn admits_direct_experience_write() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        let m = mem_with("my own observation", Provenance::DirectExperience);
        let id = store
            .add(m)
            .await
            .expect("direct experience must be admitted");
        // The write ended up in the inner storage.
        assert_eq!(store.len().await.expect("len"), 1);
        assert!(store.get(id).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn rejects_low_trust_external_write() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        let m = mem_with("untrusted external claim", Provenance::external("web", 0.1));
        let err = store
            .add(m)
            .await
            .expect_err("low-trust external must be rejected");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        // Nothing ended up in the inner storage.
        assert!(store.is_empty().await.expect("empty"));
        // The error message mentions the threshold.
        assert!(err.to_string().contains("provenance rejected"));
        assert!(err.to_string().contains("0.6"));
    }

    #[tokio::test]
    async fn admits_high_trust_external_write() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        let m = mem_with(
            "well-sourced external fact",
            Provenance::external("web", 0.9),
        );
        store
            .add(m)
            .await
            .expect("high-trust external must be admitted");
        assert_eq!(store.len().await.expect("len"), 1);
    }

    #[tokio::test]
    async fn admits_derived_write() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.99));
        let sources = vec![MessageId::new(), MessageId::new()];
        let m = mem_with("a reflection", Provenance::derived(sources));
        // Derived passes even when the threshold is very high.
        store.add(m).await.expect("derived must be admitted");
        assert_eq!(store.len().await.expect("len"), 1);
    }

    #[tokio::test]
    async fn update_rejects_low_trust_external() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        // First write a trusted memory.
        let mut m = mem_with("originally trusted", Provenance::DirectExperience);
        let id = store.add(m.clone()).await.expect("add");
        // Try to update it to a low-trust external → rejected.
        m.provenance = Provenance::external("web", 0.05);
        let err = store
            .update(m)
            .await
            .expect_err("update to low-trust external must be rejected");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        // The original remained unchanged.
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.provenance, Provenance::DirectExperience);
    }

    #[tokio::test]
    async fn update_admits_trusted_change() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        let mut m = mem_with("editable", Provenance::DirectExperience);
        let id = store.add(m.clone()).await.expect("add");
        m.content = "edited content".into();
        store.update(m).await.expect("trusted update must pass");
        let got = store.get(id).await.expect("get").expect("present");
        assert_eq!(got.content, "edited content");
    }

    #[tokio::test]
    async fn passthrough_methods_delegate() {
        let store = GatedMemoryStore::with_default_gate(LocalJsonStore::in_memory());
        // add (trusted) → get → reinforce → set_status → retrieve → run_decay
        let m = mem_with("rust memory engine", Provenance::DirectExperience);
        let id = store.add(m).await.expect("add");

        let before = store.get(id).await.expect("g").expect("p").importance;
        store.reinforce(id, time::now()).await.expect("reinforce");
        let after = store.get(id).await.expect("g").expect("p");
        assert!(after.importance > before);

        store
            .set_status(id, MemoryStatus::Archived)
            .await
            .expect("set_status");
        assert_eq!(
            store.get(id).await.expect("g").expect("p").status,
            MemoryStatus::Archived
        );

        let ctx = RetrievalContext::new("rust memory");
        let hits = store.retrieve(&ctx, time::now()).await.expect("retrieve");
        assert!(!hits.is_empty());

        let report = store
            .run_decay(DecayThresholds::default(), time::now())
            .await
            .expect("run_decay");
        assert_eq!(report.scanned, 1);

        // all + len + is_empty are delegated.
        assert_eq!(store.all().await.expect("all").len(), 1);
        assert_eq!(store.len().await.expect("len"), 1);
        assert!(!store.is_empty().await.expect("is_empty"));
    }

    #[tokio::test]
    async fn with_default_gate_uses_half_threshold() {
        let store = GatedMemoryStore::with_default_gate(LocalJsonStore::in_memory());
        assert_eq!(store.gate().min_trust(), 0.5);
        // Exactly at the threshold → admitted (>=).
        let m = mem_with("on the boundary", Provenance::external("tool", 0.5));
        store.add(m).await.expect("boundary trust admitted");
        // Just below → rejected.
        let low = mem_with("just below", Provenance::external("tool", 0.49));
        assert!(store.add(low).await.is_err());
    }

    #[tokio::test]
    async fn into_inner_returns_wrapped_store() {
        let store = GatedMemoryStore::with_default_gate(LocalJsonStore::in_memory());
        store
            .add(mem_with("kept", Provenance::DirectExperience))
            .await
            .expect("add");
        let inner = store.into_inner();
        // The inner storage retained the written memory.
        assert_eq!(inner.len().await.expect("len"), 1);
    }
}

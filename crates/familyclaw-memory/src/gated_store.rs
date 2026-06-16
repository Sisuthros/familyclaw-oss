//! Provenance-vahdittu muistitallennus ([`GatedMemoryStore`]).
//!
//! [`GatedMemoryStore`] kietoo minkä tahansa [`MemoryStore`]-toteutuksen ja
//! pakottaa [`ProvenanceGate`]-myrkytyssuojan **kirjoitushetkellä**: ennen kuin
//! muisto pääsee sisempään tallennukseen, sen alkuperä punnitaan portilla.
//! Matalan luottamuksen ulkoinen lähde ([`Provenance::External`] jonka `trust`
//! alittaa portin kynnyksen) hylätään, jolloin se ei pääse saastuttamaan
//! myöhempää haetua (*Sleeper Memory Poisoning* -suoja, kts.
//! [`crate::provenance`]).
//!
//! Suora kokemus ([`Provenance::DirectExperience`]) ja johdetut muistot
//! ([`Provenance::Derived`]) pääsevät aina läpi — vain ulkoiset väitteet
//! punnitaan.
//!
//! ## Suunnittelu
//! - **Additiivinen:** ei muuta [`MemoryStore`]-traitia eikä
//!   [`LocalJsonStore`]-toteutusta. Vahti on uusi, valinnainen kerros.
//! - **Läpinäkyvä:** kaikki muut metodit ([`get`](MemoryStore::get),
//!   [`retrieve`](MemoryStore::retrieve), [`run_decay`](MemoryStore::run_decay)
//!   jne.) delegoidaan sellaisenaan sisempään tallennukseen.
//! - **Kirjoitusportti:** vain [`add`](MemoryStore::add) ja
//!   [`update`](MemoryStore::update) punnitaan. Olemassa olevan muiston tilan
//!   vahvistus/elinkaarisiirto ei tuo uutta alkuperää, joten niitä ei punnita
//!   uudelleen.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_memory::{
//!     GatedMemoryStore, LocalJsonStore, Memory, MemoryStore, Provenance, ProvenanceGate,
//! };
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
//!
//! // Suora kokemus pääsee aina.
//! let trusted = Memory::builder("I saw this myself")
//!     .provenance(Provenance::DirectExperience)
//!     .build();
//! store.add(trusted).await?;
//!
//! // Matalan luottamuksen ulkoinen väite hylätään.
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

/// Type-erased future, sama muoto kuin [`MemoryStore`]-traitin metodeilla.
/// Elinaika `'a` kaappaa `&self`-lainan, jotta palautettu future voi viitata
/// `self`:iin (ja siten sisempään tallennukseen).
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Provenance-vahdittu kääre [`MemoryStore`]-toteutuksen ympärille.
///
/// Pakottaa [`ProvenanceGate`]-myrkytyssuojan kirjoitushetkellä ja delegoi
/// kaiken muun sisempään tallennukseen. Luo joko eksplisiittisellä portilla
/// ([`new`](GatedMemoryStore::new)) tai oletusportilla
/// ([`with_default_gate`](GatedMemoryStore::with_default_gate)).
#[derive(Debug)]
pub struct GatedMemoryStore<S: MemoryStore> {
    /// Kääritty sisempi tallennus johon hyväksytyt kirjoitukset delegoidaan.
    inner: S,
    /// Alkuperä-portti joka punnitsee jokaisen kirjoituksen alkuperän.
    gate: ProvenanceGate,
}

impl<S: MemoryStore> GatedMemoryStore<S> {
    /// Kietoo `inner`-tallennuksen annetulla portilla.
    #[must_use]
    pub fn new(inner: S, gate: ProvenanceGate) -> Self {
        Self { inner, gate }
    }

    /// Kietoo `inner`-tallennuksen oletusportilla
    /// ([`ProvenanceGate::default`], kynnys `0.5`).
    #[must_use]
    pub fn with_default_gate(inner: S) -> Self {
        Self {
            inner,
            gate: ProvenanceGate::default(),
        }
    }

    /// Portti joka punnitsee kirjoitusten alkuperän.
    #[must_use]
    pub const fn gate(&self) -> &ProvenanceGate {
        &self.gate
    }

    /// Kääritty sisempi tallennus (lukuoikeus).
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Purkaa kääreen ja palauttaa sisemmän tallennuksen.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Rakentaa hylkäysvirheen kun alkuperä ei läpäise porttia.
    fn rejected(&self) -> FamilyClawError {
        FamilyClawError::invalid_input(format!(
            "provenance rejected: source trust below gate threshold {}",
            self.gate.min_trust()
        ))
    }
}

impl<S: MemoryStore> MemoryStore for GatedMemoryStore<S> {
    fn add(&self, memory: Memory) -> BoxFuture<'_, Result<MessageId>> {
        // Punnitse alkuperä ENNEN delegointia: matalan luottamuksen ulkoinen
        // lähde ei saa edes yrittää kirjoittaa sisempään tallennukseen.
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
        // Päivitys voi tuoda uuden alkuperän → punnitaan samoin kuin add.
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
    // Osa testeistä vertaa tarkkoja f32-vakioita (portin kynnys) — tarkka
    // vertailu on tässä tarkoituksellista ja turvallista.
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
        // Kääreen pitää säilyttää Send + Sync, jotta se kelpaa monisäikeiseen
        // tokio-ajoon siinä missä sisempi tallennuskin.
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
        // Kirjoitus päätyi sisempään tallennukseen.
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
        // Mikään ei päätynyt sisempään tallennukseen.
        assert!(store.is_empty().await.expect("empty"));
        // Virheviesti mainitsee kynnyksen.
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
        // Johdettu pääsee vaikka kynnys on hyvin korkea.
        store.add(m).await.expect("derived must be admitted");
        assert_eq!(store.len().await.expect("len"), 1);
    }

    #[tokio::test]
    async fn update_rejects_low_trust_external() {
        let store = GatedMemoryStore::new(LocalJsonStore::in_memory(), ProvenanceGate::new(0.6));
        // Kirjoita ensin luotettu muisto sisään.
        let mut m = mem_with("originally trusted", Provenance::DirectExperience);
        let id = store.add(m.clone()).await.expect("add");
        // Yritä päivittää se matalan luottamuksen ulkoiseksi → hylätään.
        m.provenance = Provenance::external("web", 0.05);
        let err = store
            .update(m)
            .await
            .expect_err("update to low-trust external must be rejected");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        // Alkuperäinen säilyi koskemattomana.
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
        // add (luotettu) → get → reinforce → set_status → retrieve → run_decay
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

        // all + len + is_empty delegoituvat.
        assert_eq!(store.all().await.expect("all").len(), 1);
        assert_eq!(store.len().await.expect("len"), 1);
        assert!(!store.is_empty().await.expect("is_empty"));
    }

    #[tokio::test]
    async fn with_default_gate_uses_half_threshold() {
        let store = GatedMemoryStore::with_default_gate(LocalJsonStore::in_memory());
        assert_eq!(store.gate().min_trust(), 0.5);
        // Täsmälleen kynnyksellä → hyväksytään (>=).
        let m = mem_with("on the boundary", Provenance::external("tool", 0.5));
        store.add(m).await.expect("boundary trust admitted");
        // Aavistuksen alle → hylätään.
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
        // Sisempi tallennus säilytti kirjoitetun muiston.
        assert_eq!(inner.len().await.expect("len"), 1);
    }
}

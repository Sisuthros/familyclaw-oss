//! # familyclaw-sandbox
//!
//! Eristetty koodisuoritus FamilyClaw-alustalle: WASM-pohjainen sandbox jossa
//! **polttoaine (fuel) pakottaa suorituskaton** ja **kyvykkyysmalli rajaa
//! pääsyn** (design §2 turva). Tämä on KERROS A:n (OSS) crate — se ei
//! kovakoodaa perheenjäsenten sieluja, avaimia eikä polkuja.
//!
//! ## Rakenne
//! Crate on tarkoituksella kerroksittainen jotta turvalogiikka on testattavaa
//! ilman raskasta wasmtime-riippuvuutta:
//!
//! - [`CodeSandbox`] — backend-riippumaton rajapinta
//!   ([`execute`](CodeSandbox::execute)).
//! - [`Capability`] / [`CapabilitySet`] — "deny by default" -kyvykkyysmalli
//!   (verkko, tiedostot, ympäristömuuttujat).
//! - [`FuelLimit`] / [`FuelMeter`] — polttoainebudjetti ja sen mittaus.
//! - [`NoopSandbox`] — **oletustoteutus** joka ei aja koodia (palauttaa
//!   [`SandboxError::NotImplemented`]). Turvallinen kun wasmtimea ei tarvita.
//! - `WasmtimeSandbox` — oikea wasmtime-pohjainen toteutus
//!   **`wasmtime`-featuren takana** (ks. alla).
//!
//! ## Feature-flagit
//! - **`wasmtime`** (ei oletuksena): kytkee
//!   `WasmtimeSandbox`-toteutuksen. wasmtime on iso
//!   riippuvuus (Cranelift + JIT), joten se on optional ettei se hidasta koko
//!   workspacen buildia. Ilman tätä featurea vain [`NoopSandbox`] on saatavilla.
//!
//! ```toml
//! [dependencies]
//! familyclaw-sandbox = { version = "0.1", features = ["wasmtime"] }
//! ```
//!
//! ## Esimerkki (oletus, ilman wasmtimea)
//! ```
//! use familyclaw_sandbox::{CodeSandbox, NoopSandbox, SandboxRequest};
//!
//! let sandbox = NoopSandbox::new();
//! assert!(!sandbox.can_execute());
//!
//! let request = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
//! // NoopSandbox validoi pyynnön mutta ei aja koodia.
//! let result = sandbox.execute(&request);
//! assert!(result.is_err());
//! ```
//!
//! ## Turvaperiaatteet
//! - **Deny by default:** ilman myönnettyä [`Capability`]:ia ajettavalla
//!   koodilla ei ole verkkoa, tiedostoja eikä ympäristömuuttujia.
//! - **Polttoaine pakottaa rajan:** ikuinen silmukka keskeytyy
//!   [`SandboxError::FuelExhausted`]:lla.
//! - **Determinismi:** sama [`SandboxRequest`] tuottaa saman tuloksen
//!   (durable-replayn edellytys).
//!
//! ## Containment-vaatimukset (2604.23425) — missä crate pakottaa kunkin
//!
//! Paperi johtaa 698:n incidentin analyysistä viisi arkkitehtonista
//! vaatimusta eristetylle koodisuoritukselle. Tämä crate kartoittuu niihin
//! seuraavasti:
//!
//! 1. **Resurssirajat (resource limits)** — [`FuelLimit`] / [`FuelMeter`].
//!    Polttoainebudjetti katkaisee ikuiset silmukat ja resurssien
//!    väärinkäytön; ylitys palauttaa [`SandboxError::FuelExhausted`].
//! 2. **Verkkoeristys (network isolation)** — [`CapabilitySet`]
//!    ([`allows_network_host`](CapabilitySet::allows_network_host)).
//!    Oletuksena ([`deny_all`](CapabilitySet::deny_all)) verkkoa ei ole;
//!    pääsy vain eksplisiittisesti myönnettyihin isäntiin.
//! 3. **Tiedostojärjestelmän eristys (filesystem sandboxing)** —
//!    [`CapabilitySet`]
//!    ([`allows_read_path`](CapabilitySet::allows_read_path)).
//!    Komponenttitason etuliitevertailu rajaa luvun myönnettyihin
//!    alipuihin; muu polkupääsy evätään.
//! 4. **Kyvykkyyspääsy (capability access)** — [`Capability`] /
//!    [`CapabilitySet`] kokonaisuutena: additiivinen "deny by default"
//!    -malli, jonka [`validate`](CapabilitySet::validate) hylkää
//!    huonosti muodostetut myönnöt.
//! 5. **Tarkastusloki (audit logging)** — [`AuditLog`] /
//!    [`AuditedCapabilities`]. Append-only-loki kirjaa jokaisen
//!    kyvykkyystarkistuksen (myönnetty/evätty) sekä suoritusten alun ja
//!    lopun. Kytketään **valinnaisena** koukkuna muuttamatta olemassa
//!    olevien tyyppien julkista rajapintaa.
//!
//! Lisäksi [`replay`](mod@replay) toteuttaa LOOP-mekanismin (2605.14237): suoritus
//! tallennetaan [`ExecutionTrace`]:ksi ja toistetaan deterministisesti
//! pelkästä lokista, mikä mahdollistaa containment-tapahtumien bitintarkan
//! jälkitarkastelun ilman alkuperäistä backendia.

pub mod audit;
pub mod capability;
pub mod error;
pub mod fuel;
pub mod noop;
pub mod replay;
pub mod sandbox;

#[cfg(feature = "wasmtime")]
pub mod wasmtime_backend;

pub use audit::{AuditEntry, AuditLog, AuditedCapabilities, CapabilityCheck};
pub use capability::{Capability, CapabilitySet};
pub use error::{Result, SandboxError};
pub use fuel::{FuelLimit, FuelMeter};
pub use noop::NoopSandbox;
pub use replay::{replay, ExecutionTrace, Outcome, TraceEvent};
pub use sandbox::{CodeSandbox, SandboxOutput, SandboxRequest, SandboxResult};

#[cfg(feature = "wasmtime")]
pub use wasmtime_backend::WasmtimeSandbox;

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Palauttaa oletussandboxin laatikoituna trait-objektina.
///
/// `wasmtime`-featuren kanssa tämä on
/// `WasmtimeSandbox`; ilman sitä
/// [`NoopSandbox`]. Tämä antaa kutsujalle backend-riippumattoman tavan saada
/// "paras saatavilla oleva" sandbox.
///
/// # Errors
/// [`SandboxError::Setup`] jos `wasmtime`-backendin alustus epäonnistuu.
/// Noop-tapauksessa ei koskaan epäonnistu.
pub fn default_sandbox() -> Result<Box<dyn CodeSandbox>> {
    #[cfg(feature = "wasmtime")]
    {
        Ok(Box::new(wasmtime_backend::WasmtimeSandbox::new()?))
    }
    #[cfg(not(feature = "wasmtime"))]
    {
        Ok(Box::new(NoopSandbox::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // Varmistaa että julkinen pinta on saatavilla juuresta.
        let _cap: Capability = Capability::network("h");
        let _caps: CapabilitySet = CapabilitySet::deny_all();
        let _limit: FuelLimit = FuelLimit::default();
        let _meter: FuelMeter = FuelMeter::default();
        let _req: SandboxRequest = SandboxRequest::new(vec![1]);
        let _out: SandboxOutput = SandboxOutput::new(vec![], 0);
        let _err: SandboxError = SandboxError::capability("x");
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());

        let sandbox: NoopSandbox = NoopSandbox::new();
        let _name = sandbox.backend_name();
    }

    #[test]
    fn default_sandbox_is_constructible_and_usable() {
        let sandbox = default_sandbox().expect("default sandbox builds");
        // Pyyntö validoidaan riippumatta backendista.
        let bad = SandboxRequest::new(Vec::<u8>::new());
        assert!(sandbox.execute(&bad).is_err());
    }

    #[cfg(not(feature = "wasmtime"))]
    #[test]
    fn default_sandbox_is_noop_without_feature() {
        let sandbox = default_sandbox().expect("noop builds");
        assert_eq!(sandbox.backend_name(), "noop");
        assert!(!sandbox.can_execute());
    }

    #[cfg(feature = "wasmtime")]
    #[test]
    fn default_sandbox_is_wasmtime_with_feature() {
        let sandbox = default_sandbox().expect("wasmtime builds");
        assert_eq!(sandbox.backend_name(), "wasmtime");
        assert!(sandbox.can_execute());
    }
}

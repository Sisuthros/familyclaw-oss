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
//!   [`WasmtimeSandbox`](crate::WasmtimeSandbox)-toteutuksen. wasmtime on iso
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

pub mod capability;
pub mod error;
pub mod fuel;
pub mod noop;
pub mod sandbox;

#[cfg(feature = "wasmtime")]
pub mod wasmtime_backend;

pub use capability::{Capability, CapabilitySet};
pub use error::{Result, SandboxError};
pub use fuel::{FuelLimit, FuelMeter};
pub use noop::NoopSandbox;
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
/// [`WasmtimeSandbox`](crate::WasmtimeSandbox); ilman sitä
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

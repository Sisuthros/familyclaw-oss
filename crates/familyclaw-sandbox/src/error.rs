//! Sandbox-kohtaiset virhetyypit.
//!
//! Sandboxilla on oma virhetyyppi [`SandboxError`] joka kantaa
//! sandbox-domainin tarkat virheluokat (polttoaineen loppuminen,
//! kyvykkyysrikkomus, backendin puuttuminen). Se muuntuu tarvittaessa
//! alustan keskitettyyn [`familyclaw_core::FamilyClawError`]-tyyppiin
//! [`From`]-toteutuksella.

use thiserror::Error;

/// Sandboxin virhetyyppi.
///
/// `#[non_exhaustive]` jotta uusia variantteja voi lisätä rikkomatta
/// downstream-koodia (esim. uusia backendejä tai turvaluokkia).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// Polttoaine loppui kesken suorituksen.
    #[error("fuel exhausted: budget {budget}, required {required}")]
    FuelExhausted {
        /// Käytettävissä ollut polttoainebudjetti.
        budget: u64,
        /// Kulutus joka olisi tarvittu (ylitti budjetin).
        required: u64,
    },

    /// Ajettava koodi rikkoi kyvykkyysrajoitusta tai kyvykkyysjoukko oli
    /// kelvoton.
    #[error("capability violation: {0}")]
    Capability(String),

    /// Pyydetty toiminto ei ole toteutettu tässä backendissä.
    ///
    /// Oletus-[`NoopSandbox`](crate::NoopSandbox) palauttaa tämän:
    /// oikea suoritus vaatii `wasmtime`-featuren.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// Sandboxin alustus epäonnistui (esim. WASM-moduulin lataus tai
    /// linkitys).
    #[error("sandbox setup failed: {0}")]
    Setup(String),

    /// Suoritus epäonnistui muusta syystä (esim. WASM-trap, paitsi fuel).
    #[error("execution failed: {0}")]
    Execution(String),
}

impl SandboxError {
    /// Rakentaa [`SandboxError::FuelExhausted`]-variantin.
    #[must_use]
    pub const fn fuel_exhausted(budget: u64, required: u64) -> Self {
        Self::FuelExhausted { budget, required }
    }

    /// Rakentaa [`SandboxError::Capability`]-variantin.
    pub fn capability(msg: impl Into<String>) -> Self {
        Self::Capability(msg.into())
    }

    /// Rakentaa [`SandboxError::NotImplemented`]-variantin.
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self::NotImplemented(msg.into())
    }

    /// Rakentaa [`SandboxError::Setup`]-variantin.
    pub fn setup(msg: impl Into<String>) -> Self {
        Self::Setup(msg.into())
    }

    /// Rakentaa [`SandboxError::Execution`]-variantin.
    pub fn execution(msg: impl Into<String>) -> Self {
        Self::Execution(msg.into())
    }

    /// Onko kyseessä polttoaineen loppuminen.
    #[must_use]
    pub const fn is_fuel_exhausted(&self) -> bool {
        matches!(self, Self::FuelExhausted { .. })
    }

    /// Onko kyseessä toteuttamaton toiminto.
    #[must_use]
    pub const fn is_not_implemented(&self) -> bool {
        matches!(self, Self::NotImplemented(_))
    }
}

impl From<SandboxError> for familyclaw_core::FamilyClawError {
    /// Muuntaa sandbox-virheen alustan keskitettyyn virhetyyppiin.
    ///
    /// Kaikki sandbox-virheet kartoittuvat [`FamilyClawError::Bus`]:iin tai
    /// [`FamilyClawError::InvalidInput`]:iin sopivimmin: polttoaine ja
    /// suoritusvirheet ovat ajonaikaisia (Bus), kyvykkyys- ja
    /// toteutusvirheet kelpaavat syöte-/kelpoisuusvirheiksi.
    fn from(err: SandboxError) -> Self {
        match err {
            SandboxError::Capability(_) | SandboxError::NotImplemented(_) => {
                familyclaw_core::FamilyClawError::invalid_input(err.to_string())
            }
            other => familyclaw_core::FamilyClawError::bus(other.to_string()),
        }
    }
}

/// Sandboxin tulostyyppi: [`std::result::Result`] jonka virhe on
/// [`SandboxError`].
pub type Result<T> = std::result::Result<T, SandboxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuel_exhausted_display_and_predicate() {
        let err = SandboxError::fuel_exhausted(100, 150);
        assert!(err.is_fuel_exhausted());
        assert_eq!(
            err.to_string(),
            "fuel exhausted: budget 100, required 150"
        );
    }

    #[test]
    fn capability_constructor() {
        let err = SandboxError::capability("no network");
        assert!(matches!(err, SandboxError::Capability(_)));
        assert_eq!(err.to_string(), "capability violation: no network");
    }

    #[test]
    fn not_implemented_predicate() {
        let err = SandboxError::not_implemented("need wasmtime feature");
        assert!(err.is_not_implemented());
        assert!(err.to_string().starts_with("not implemented:"));
    }

    #[test]
    fn setup_and_execution_constructors() {
        assert_eq!(
            SandboxError::setup("bad module").to_string(),
            "sandbox setup failed: bad module"
        );
        assert_eq!(
            SandboxError::execution("trap").to_string(),
            "execution failed: trap"
        );
    }

    #[test]
    fn converts_to_core_invalid_input_for_capability() {
        let err: familyclaw_core::FamilyClawError =
            SandboxError::capability("denied").into();
        assert!(matches!(
            err,
            familyclaw_core::FamilyClawError::InvalidInput(_)
        ));
    }

    #[test]
    fn converts_to_core_invalid_input_for_not_implemented() {
        let err: familyclaw_core::FamilyClawError =
            SandboxError::not_implemented("x").into();
        assert!(matches!(
            err,
            familyclaw_core::FamilyClawError::InvalidInput(_)
        ));
    }

    #[test]
    fn converts_to_core_bus_for_runtime_errors() {
        let fuel: familyclaw_core::FamilyClawError =
            SandboxError::fuel_exhausted(1, 2).into();
        assert!(matches!(fuel, familyclaw_core::FamilyClawError::Bus(_)));

        let exec: familyclaw_core::FamilyClawError =
            SandboxError::execution("trap").into();
        assert!(matches!(exec, familyclaw_core::FamilyClawError::Bus(_)));
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<SandboxError>();
    }
}

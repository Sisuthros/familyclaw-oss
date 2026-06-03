//! Oletustoteutus [`NoopSandbox`] joka ei aja koodia.
//!
//! Tämä on turvallinen oletus kun `wasmtime`-featurea ei ole käännetty:
//! mikään koodi ei aja, joten mitään hyökkäyspintaa ei ole. Suoritusyritys
//! palauttaa [`SandboxError::NotImplemented`]:n selkeällä viestillä joka
//! ohjaa kytkemään `wasmtime`-featuren oikeaa suoritusta varten.
//!
//! `NoopSandbox` **validoi silti pyynnön** (koodi ei tyhjä, kyvykkyydet
//! hyvinmuodostetut) ennen kuin palauttaa `NotImplemented`. Näin kutsujan
//! pyyntövirheet löytyvät myös ilman wasmtime-backendia.

use crate::error::SandboxError;
use crate::sandbox::{CodeSandbox, SandboxRequest, SandboxResult};

/// Sandbox-toteutus joka ei suorita mitään koodia.
///
/// Käytä kun:
/// - `wasmtime`-featurea ei ole päällä, tai
/// - halutaan eksplisiittisesti kieltää kaikki koodisuoritus.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct NoopSandbox;

impl NoopSandbox {
    /// Luo uuden [`NoopSandbox`]-instanssin.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CodeSandbox for NoopSandbox {
    fn execute(&self, request: &SandboxRequest) -> SandboxResult {
        // Validoi pyyntö ensin: kutsujan virheet löytyvät myös ilman backendia.
        request.validate()?;
        Err(SandboxError::not_implemented(
            "NoopSandbox does not execute code; enable the `wasmtime` feature \
             for real sandboxed execution",
        ))
    }

    fn backend_name(&self) -> &'static str {
        "noop"
    }

    fn can_execute(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, CapabilitySet};
    use crate::fuel::FuelLimit;

    #[test]
    fn reports_noop_backend_and_cannot_execute() {
        let sandbox = NoopSandbox::new();
        assert_eq!(sandbox.backend_name(), "noop");
        assert!(!sandbox.can_execute());
    }

    #[test]
    fn execute_returns_not_implemented_for_valid_request() {
        let sandbox = NoopSandbox::new();
        let req = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
        let err = sandbox.execute(&req).expect_err("noop never executes");
        assert!(err.is_not_implemented());
        assert!(err.to_string().contains("wasmtime"));
    }

    #[test]
    fn execute_validates_before_reporting_not_implemented() {
        let sandbox = NoopSandbox::new();
        // Tyhjä koodi → setup-virhe, EI NotImplemented.
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = sandbox.execute(&req).expect_err("empty code rejected");
        assert!(!err.is_not_implemented());
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn execute_rejects_bad_capabilities_before_not_implemented() {
        let sandbox = NoopSandbox::new();
        let bad = CapabilitySet::deny_all().with(Capability::env_var("  "));
        let req = SandboxRequest::new(vec![1]).with_capabilities(bad);
        let err = sandbox.execute(&req).expect_err("bad caps rejected");
        assert!(!err.is_not_implemented());
    }

    #[test]
    fn usable_as_trait_object() {
        let sandbox: Box<dyn CodeSandbox> = Box::new(NoopSandbox::default());
        let req = SandboxRequest::new(vec![1]).with_fuel_limit(FuelLimit::limited(10));
        assert!(sandbox.execute(&req).is_err());
        assert_eq!(sandbox.backend_name(), "noop");
    }
}

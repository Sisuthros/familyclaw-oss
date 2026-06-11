//! Roolipohjainen pääsynvalvonta ([`RbacPolicy`]) agenttien kyvykkyyksille.
//!
//! Tämä on **syvyyspuolustusta** wasmtime-hiekkalaatikon päällä: vaikka
//! sandbox rajaa mitä koodi *voi* tehdä, RBAC rajaa mitä kullakin roolilla on
//! *lupa* tehdä. Politiikka kuvaa roolista ([`AgentRole`]) sallittuihin
//! kyvykkyystunnisteisiin (esim. `"browser"`, `"system.run"`).
//!
//! ## Periaatteet
//! - **Oletuksena kielto.** Tyhjä politiikka kieltää kaiken. Luvat lisätään
//!   eksplisiittisesti [`RbacPolicy::allow`]-rakentajalla.
//! - **Deterministinen.** Tarkastus on puhdas funktio politiikan tilasta.
//! - **OSS-raja:** roolit ja kyvykkyydet ovat geneerisiä tunnisteita, ei
//!   sieluja eikä avaimia.

use std::collections::{HashMap, HashSet};

use familyclaw_bridge::AgentRole;
use familyclaw_core::{FamilyClawError, Result};

/// RBAC-tarkastuksen virhe.
///
/// Erillinen tyyppi joka muuntuu [`FamilyClawError`]:ksi (`?`-operaattorilla),
/// jotta kutsuja saa selkeän pääsynvalvontavirheen mutta voi silti yhdistää
/// sen alustan keskitettyyn virhetyyppiin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbacError {
    /// Roolilla ei ole oikeutta annettuun kyvykkyyteen.
    Denied {
        /// Rooli jolle pääsy evättiin.
        role: AgentRole,
        /// Kyvykkyys jota yritettiin käyttää.
        capability: String,
    },
}

impl std::fmt::Display for RbacError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RbacError::Denied { role, capability } => {
                write!(
                    f,
                    "rbac: role {role:?} is not permitted capability '{capability}'"
                )
            }
        }
    }
}

impl std::error::Error for RbacError {}

impl From<RbacError> for FamilyClawError {
    fn from(err: RbacError) -> Self {
        FamilyClawError::invalid_input(err.to_string())
    }
}

/// Roolipohjainen pääsynvalvontapolitiikka.
///
/// Kuvaa kullekin [`AgentRole`]:lle joukon sallittuja kyvykkyystunnisteita.
/// Oletuksena (tyhjä politiikka) kaikki on kielletty; luvat lisätään
/// [`allow`]-rakentajalla.
///
/// [`allow`]: RbacPolicy::allow
#[derive(Debug, Clone, Default)]
pub struct RbacPolicy {
    allowed: HashMap<AgentRole, HashSet<String>>,
}

impl RbacPolicy {
    /// Luo tyhjän politiikan (kaikki kielletty).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää luvan: rooli `role` saa käyttää kyvykkyyttä `capability`.
    ///
    /// Rakentaja-tyyli (palauttaa `self`), joten lupia voi ketjuttaa.
    /// Idempotentti: saman luvan lisääminen kahdesti ei muuta tilaa.
    #[must_use]
    pub fn allow(mut self, role: AgentRole, capability: impl Into<String>) -> Self {
        self.allowed
            .entry(role)
            .or_default()
            .insert(capability.into());
        self
    }

    /// Lisää luvan paikan päällä (ei-rakentaja-variantti).
    pub fn grant(&mut self, role: AgentRole, capability: impl Into<String>) {
        self.allowed
            .entry(role)
            .or_default()
            .insert(capability.into());
    }

    /// Poistaa luvan. Palauttaa `true` jos lupa oli olemassa.
    pub fn revoke(&mut self, role: AgentRole, capability: &str) -> bool {
        self.allowed
            .get_mut(&role)
            .is_some_and(|set| set.remove(capability))
    }

    /// Onko roolilla lupa annettuun kyvykkyyteen (boolitarkastus, ei virhettä).
    #[must_use]
    pub fn is_allowed(&self, role: AgentRole, capability: &str) -> bool {
        self.allowed
            .get(&role)
            .is_some_and(|set| set.contains(capability))
    }

    /// Tarkastaa luvan ja palauttaa virheen jos pääsy evätään.
    ///
    /// # Errors
    /// [`RbacError::Denied`] jos roolilla ei ole oikeutta kyvykkyyteen.
    pub fn check(&self, role: AgentRole, capability: &str) -> std::result::Result<(), RbacError> {
        if self.is_allowed(role, capability) {
            Ok(())
        } else {
            Err(RbacError::Denied {
                role,
                capability: capability.to_string(),
            })
        }
    }

    /// Tarkastaa luvan ja muuntaa virheen alustan [`FamilyClawError`]:ksi.
    ///
    /// Mukavuusmetodi kun kutsuja työskentelee [`Result`]-tyypin kanssa.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] (kääritty [`RbacError`]) jos pääsy
    /// evätään.
    pub fn check_core(&self, role: AgentRole, capability: &str) -> Result<()> {
        self.check(role, capability).map_err(FamilyClawError::from)
    }

    /// Palauttaa roolin sallitut kyvykkyydet aakkosjärjestyksessä
    /// (deterministinen, sopii tarkastukseen/lokitukseen).
    #[must_use]
    pub fn capabilities_for(&self, role: AgentRole) -> Vec<String> {
        let mut out: Vec<String> = self
            .allowed
            .get(&role)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_policy_denies_everything() {
        let policy = RbacPolicy::new();
        assert!(!policy.is_allowed(AgentRole::Executor, "browser"));
        assert!(policy.check(AgentRole::Executor, "browser").is_err());
    }

    #[test]
    fn allow_grants_specific_capability() {
        let policy = RbacPolicy::new()
            .allow(AgentRole::Executor, "system.run")
            .allow(AgentRole::Executor, "browser");

        assert!(policy.is_allowed(AgentRole::Executor, "system.run"));
        assert!(policy.is_allowed(AgentRole::Executor, "browser"));
        assert!(policy.check(AgentRole::Executor, "system.run").is_ok());

        // Toinen rooli ei peri lupia.
        assert!(!policy.is_allowed(AgentRole::Scout, "system.run"));
    }

    #[test]
    fn check_returns_denied_error_with_context() {
        let policy = RbacPolicy::new();
        let err = policy
            .check(AgentRole::Scout, "system.run")
            .expect_err("denied");
        assert_eq!(
            err,
            RbacError::Denied {
                role: AgentRole::Scout,
                capability: "system.run".to_string(),
            }
        );
        assert!(err.to_string().contains("system.run"));
    }

    #[test]
    fn rbac_error_converts_to_family_claw_error() {
        let err = RbacError::Denied {
            role: AgentRole::FieldOperator,
            capability: "device.write".to_string(),
        };
        let core: FamilyClawError = err.into();
        assert!(matches!(core, FamilyClawError::InvalidInput(_)));
        assert!(core.to_string().contains("device.write"));
    }

    #[test]
    fn check_core_propagates_via_question_mark() {
        fn guarded(policy: &RbacPolicy) -> Result<()> {
            policy.check_core(AgentRole::Strategy, "deploy")?;
            Ok(())
        }
        let allow = RbacPolicy::new().allow(AgentRole::Strategy, "deploy");
        assert!(guarded(&allow).is_ok());

        let deny = RbacPolicy::new();
        assert!(guarded(&deny).is_err());
    }

    #[test]
    fn grant_and_revoke_in_place() {
        let mut policy = RbacPolicy::new();
        policy.grant(AgentRole::Executor, "browser");
        assert!(policy.is_allowed(AgentRole::Executor, "browser"));

        assert!(policy.revoke(AgentRole::Executor, "browser"));
        assert!(!policy.is_allowed(AgentRole::Executor, "browser"));
        // Toinen revoke samalle: ei ollut olemassa.
        assert!(!policy.revoke(AgentRole::Executor, "browser"));
    }

    #[test]
    fn allow_is_idempotent() {
        let policy = RbacPolicy::new()
            .allow(AgentRole::Scout, "read")
            .allow(AgentRole::Scout, "read");
        assert_eq!(policy.capabilities_for(AgentRole::Scout), vec!["read"]);
    }

    #[test]
    fn capabilities_for_is_sorted() {
        let policy = RbacPolicy::new()
            .allow(AgentRole::Strategy, "zeta")
            .allow(AgentRole::Strategy, "alpha")
            .allow(AgentRole::Strategy, "mu");
        assert_eq!(
            policy.capabilities_for(AgentRole::Strategy),
            vec!["alpha", "mu", "zeta"]
        );
        // Roolilla ilman lupia → tyhjä.
        assert!(policy.capabilities_for(AgentRole::Scout).is_empty());
    }
}

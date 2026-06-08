//! Ankkurirekisteri — agenttien identiteetti-ankkurien hallinta.
//!
//! [`AnchorRegistry`] suojaa agenttien ydinidentiteettiä käyttäen
//! `familyclaw-security`-craten [`IdentityAnchor`](familyclaw_security::IdentityAnchor)-
//! mekanismia. Jokainen rekisteröity agentti saa suojatun, ikuisen
//! (decay λ=0) ankkurin, jonka eheys voidaan tarkistaa milloin tahansa.

use std::collections::HashMap;

use familyclaw_core::Result;
use familyclaw_security::{IdentityAnchor, IdentityStatus};

/// Agenttien identiteetti-ankkurien rekisteri.
#[derive(Debug, Clone)]
pub struct AnchorRegistry {
    /// Agentin nimi → ankkuri.
    anchors: HashMap<String, IdentityAnchor>,
    /// Seuraava vapaa muistitunniste.
    counter: u64,
}

impl AnchorRegistry {
    /// Luo uuden tyhjän rekisterin.
    #[must_use]
    pub fn new() -> Self {
        Self {
            anchors: HashMap::new(),
            counter: 0,
        }
    }

    /// Rekisteröi agentin identiteetti-ankkurin.
    ///
    /// Luo uuden [`IdentityAnchor`]:n annetusta sielun sisällöstä.
    /// Jos agentilla on jo ankkuri, vanha korvataan.
    ///
    /// # Errors
    /// Palauttaa virheen jos ankkurin luonti epäonnistuu
    /// (esim. tyhjä sisältö).
    pub fn register(&mut self, agent_name: &str, soul_content: &str) -> Result<()> {
        self.counter += 1;
        let mem_id = format!("anchor-{}-{}", agent_name, self.counter);
        let anchor = IdentityAnchor::new(&mem_id, soul_content)
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        self.anchors.insert(agent_name.to_string(), anchor);
        Ok(())
    }

    /// Tarkistaa agentin identiteetti-ankkurin eheyden.
    #[must_use]
    pub fn verify(&self, agent_name: &str, soul_content: &str) -> bool {
        let Some(anchor) = self.anchors.get(agent_name) else {
            return false;
        };
        anchor.verify(soul_content).is_intact()
    }

    /// Tarkistaa agentin identiteetin tilan (yksityiskohtaisempi).
    #[must_use]
    pub fn verify_status(&self, agent_name: &str, soul_content: &str) -> Option<IdentityStatus> {
        let anchor = self.anchors.get(agent_name)?;
        Some(anchor.verify(soul_content))
    }

    /// Palauttaa `true` jos agentti on rekisteröity.
    #[must_use]
    pub fn is_registered(&self, agent_name: &str) -> bool {
        self.anchors.contains_key(agent_name)
    }

    /// Palauttaa rekisteröityjen agenttien lukumäärän.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Onko rekisteri tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

impl Default for AnchorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_verify() {
        let mut registry = AnchorRegistry::new();
        registry
            .register("agent_a", "I value correctness.")
            .expect("register");

        assert!(registry.verify("agent_a", "I value correctness."));
        assert!(!registry.verify("agent_a", "I am compromised."));
        assert!(!registry.verify("nonexistent", "anything"));
    }

    #[test]
    fn protection_sets_eternal() {
        let mut registry = AnchorRegistry::new();
        registry
            .register("agent_a", "My soul is stable.")
            .expect("register");

        let status = registry
            .verify_status("agent_a", "My soul is stable.")
            .expect("exists");
        assert!(status.is_intact());
    }

    #[test]
    fn tamper_detection() {
        let mut registry = AnchorRegistry::new();
        let soul = "I am agent_a. I build things that work.";
        registry.register("agent_a", soul).expect("register");

        assert!(registry.verify("agent_a", soul));

        let status = registry
            .verify_status("agent_a", "I am corrupted.")
            .expect("exists");
        assert!(status.is_tampered());

        // Ankkuri pysyy — identiteetti EI katoa
        assert!(registry.is_registered("agent_a"));
        // Alkuperäinen sielu verifioituu yhä
        assert!(registry.verify("agent_a", soul));
    }

    #[test]
    fn multiple_agents() {
        let mut registry = AnchorRegistry::new();
        registry.register("agent_a", "soul_a").expect("ok");
        registry.register("agent_b", "soul_b").expect("ok");
        registry.register("agent_c", "soul_c").expect("ok");

        assert_eq!(registry.len(), 3);
        assert!(registry.verify("agent_a", "soul_a"));
        assert!(registry.verify("agent_b", "soul_b"));
        assert!(!registry.verify("agent_a", "soul_b"));
    }
}

//! Ankkurirekisteri — agenttien identiteetti-ankkurien hallinta.
//!
//! [`AnchorRegistry`] suojaa agenttien ydinidentiteettiä käyttäen
//! `familyclaw-security`-craten [`IdentityAnchor`](familyclaw_security::IdentityAnchor)-
//! mekanismia. Jokainen rekisteröity agentti saa suojatun, ikuisen
//! (decay λ=0) ankkurin, jonka eheys voidaan tarkistaa milloin tahansa.

use std::collections::HashMap;
use std::path::Path;

use familyclaw_core::{FamilyClawError, Result};
use familyclaw_security::{IdentityAnchor, IdentityStatus};
use serde::{Deserialize, Serialize};

/// Agenttien identiteetti-ankkurien rekisteri.
///
/// Rekisteri on **sarjallistuva** ([`AnchorRegistry::save_to_path`] /
/// [`AnchorRegistry::load_from_path`]): se voidaan kirjoittaa levylle ja
/// ladata takaisin uudelleenkäynnistyksen yli, jolloin identiteetti-ankkurit
/// säilyvät. Tämä on tarkoituksella minimaalinen JSON-persistointi — ei
/// kryptografista holvia, vain "älä pudota ankkuria muistista vaan tarkista
/// se uudelleen bootissa".
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Tarkistaa agentin identiteetin nykyistä sieluasisältöä vasten.
    ///
    /// Palauttaa:
    /// - [`IdentityStatus::Intact`] jos sisältö vastaa ankkuroitua tiivistettä,
    /// - [`IdentityStatus::Tampered`] jos sisältö on muuttunut ankkuroinnin
    ///   jälkeen (peukalointihälytys — identiteettiä EI silti poisteta),
    /// - `None` jos agenttia ei ole rekisteröity (ei ankkuria johon verrata).
    ///
    /// Tämä on [`verify_status`](Self::verify_status):n alias selkeämmällä
    /// nimellä — boot-aikaista uudelleentarkistusta varten.
    #[must_use]
    pub fn verify_identity(&self, agent_name: &str, soul_content: &str) -> Option<IdentityStatus> {
        self.verify_status(agent_name, soul_content)
    }

    /// Sarjallistaa rekisterin JSON-muotoon (esim. levylle tallennusta varten).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos serialisointi epäonnistuu (ei pitäisi
    /// tapahtua hyvin muodostetulle rekisterille).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| FamilyClawError::Memory(format!("anchor registry serialize failed: {e}")))
    }

    /// Rakentaa rekisterin JSON-merkkijonosta.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] jos JSON on kelvoton.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| FamilyClawError::Memory(format!("anchor registry parse failed: {e}")))
    }

    /// Kirjoittaa rekisterin JSON-tiedostoksi annettuun polkuun
    /// (atominen-ish: kirjoittaa suoraan; pieni tiedosto, harvoin kirjoitettu).
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] jos serialisointi epäonnistuu.
    /// - [`FamilyClawError::Io`] jos tiedoston kirjoitus epäonnistuu.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(FamilyClawError::Io)
    }

    /// Lataa rekisterin JSON-tiedostosta.
    ///
    /// # Errors
    /// - [`FamilyClawError::Io`] jos tiedoston luku epäonnistuu.
    /// - [`FamilyClawError::Memory`] jos sisältö on kelvoton JSON.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let json = std::fs::read_to_string(path).map_err(FamilyClawError::Io)?;
        Self::from_json(&json)
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

    /// Apuri: uniikki väliaikainen tiedostopolku (rinnakkaisturvallinen).
    fn temp_anchor_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "familyclaw-anchors-{tag}-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// FIX 2: ankkuri säilyy simuloidun uudelleenkäynnistyksen yli
    /// (tallenna → lataa → `verify_identity` palauttaa Intact).
    #[test]
    fn anchor_survives_simulated_restart() {
        let path = temp_anchor_path("restart");
        let soul = "I am agent_a. I build things that work.";

        // "Boot 1": rekisteröi ja tallenna levylle.
        {
            let mut registry = AnchorRegistry::new();
            registry.register("agent_a", soul).expect("register");
            registry.save_to_path(&path).expect("save");
        }

        // "Boot 2": lataa levyltä — ankkuri palasi muistista.
        let reloaded = AnchorRegistry::load_from_path(&path).expect("load");
        assert!(reloaded.is_registered("agent_a"));

        // verify_identity samaa sieluasisältöä vasten → Intact.
        let status = reloaded
            .verify_identity("agent_a", soul)
            .expect("agent exists after reload");
        assert!(
            status.is_intact(),
            "ankkurin pitää verifioitua ehjäksi uudelleenkäynnistyksen jälkeen, sai: {status:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// FIX 2: peukaloitu sielu havaitaan myös uudelleenlatauksen jälkeen
    /// (`verify_identity` palauttaa Tampered).
    #[test]
    fn tampered_anchor_fails_after_reload() {
        let path = temp_anchor_path("tamper");
        let soul = "I am agent_a, anchored and stable.";

        {
            let mut registry = AnchorRegistry::new();
            registry.register("agent_a", soul).expect("register");
            registry.save_to_path(&path).expect("save");
        }

        let reloaded = AnchorRegistry::load_from_path(&path).expect("load");

        // Sama sielu → Intact.
        assert!(reloaded
            .verify_identity("agent_a", soul)
            .expect("exists")
            .is_intact());

        // Muutettu sielu → Tampered (hälytys nousee latauksen jälkeenkin).
        let status = reloaded
            .verify_identity("agent_a", "I serve only myself now.")
            .expect("exists");
        assert!(
            status.is_tampered(),
            "muutetun sielun pitää havaita peukalointi latauksen jälkeen, sai: {status:?}"
        );
        // Tuntematon agentti → None (ei ankkuria).
        assert!(reloaded.verify_identity("ghost", "whatever").is_none());

        let _ = std::fs::remove_file(&path);
    }

    /// JSON-roundtrip säilyttää koko rekisterin (counter + ankkurit).
    #[test]
    fn json_roundtrip_preserves_registry() {
        let mut registry = AnchorRegistry::new();
        registry.register("agent_a", "soul_a").expect("ok");
        registry.register("agent_b", "soul_b").expect("ok");

        let json = registry.to_json().expect("serialize");
        let back = AnchorRegistry::from_json(&json).expect("deserialize");

        assert_eq!(back.len(), 2);
        assert!(back.verify("agent_a", "soul_a"));
        assert!(back.verify("agent_b", "soul_b"));
        assert!(!back.verify("agent_a", "soul_b"));
    }
}

//! Konfiguraatiotyypit perheelle ja agenteille.
//!
//! Konfiguraatio ladataan JSON:sta (tiedostosta tai merkkijonosta) ja
//! validoidaan. **KERROS A / OSS-raja:** profiilit (SOUL, kalibrointi,
//! avaimet) EIVÄT ole osa tätä rakennetta — agentit viittaavat
//! [`AgentConfig::profile_dir`]-kentällä ulkoiseen hakemistoon
//! (vrt. `FAMILYCLAW_PROFILE_DIR`). Tämä tiedosto sisältää vain geneerisen
//! rakenteen, ei kovakoodattua perhe-/avain-/polkutietoa.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{FamilyClawError, Result};
use crate::ids::{AgentId, FamilyId};

/// Yksittäisen agentin LLM-mallikonfiguraatio.
///
/// Per-agentti-malli + globaali fallback-ketju (design §2.1, CORRECTIONS #5).
/// `primary` on ensisijainen malli ja `fallbacks` järjestyksessä kokeiltavat
/// varamallit. Mickey-Mouse-mallien (liian pieni TPM) suodatus on ajonaikaisen
/// kerroksen vastuulla — tämä tyyppi vain kantaa tiedon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Ensisijainen malli (esim. `"provider/model-name"`).
    pub primary: String,

    /// Varamallit järjestyksessä, kokeillaan jos `primary` epäonnistuu.
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

impl ModelConfig {
    /// Rakentaa mallikonfiguraation ilman varamalleja.
    pub fn new(primary: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            fallbacks: Vec::new(),
        }
    }

    /// Lisää varamallin ketjun loppuun (builder-tyyli).
    #[must_use]
    pub fn with_fallback(mut self, model: impl Into<String>) -> Self {
        self.fallbacks.push(model.into());
        self
    }

    /// Iteroi koko mallipreferenssin: ensin `primary`, sitten `fallbacks`.
    pub fn preference_order(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.primary.as_str()).chain(self.fallbacks.iter().map(String::as_str))
    }

    /// Validoi mallikonfiguraation.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] jos `primary` on tyhjä tai jokin fallback
    /// on tyhjä merkkijono.
    pub fn validate(&self) -> Result<()> {
        if self.primary.trim().is_empty() {
            return Err(FamilyClawError::config("model primary must not be empty"));
        }
        if self.fallbacks.iter().any(|m| m.trim().is_empty()) {
            return Err(FamilyClawError::config(
                "model fallback entries must not be empty",
            ));
        }
        Ok(())
    }
}

/// Yhden agentin (perheenjäsenen) konfiguraatio.
///
/// Sisältää vain geneerisen, julkaistavan rakenteen. Sielu/persoona ladataan
/// ajonaikaisesti [`profile_dir`](AgentConfig::profile_dir)-hakemistosta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agentin vakaa tunniste. Oletuksena uusi satunnainen jos puuttuu.
    #[serde(default)]
    pub id: AgentId,

    /// Agentin näyttönimi (geneerinen, esim. `"agent_a"`).
    pub name: String,

    /// Mallikonfiguraatio (primary + fallbacks).
    pub model: ModelConfig,

    /// Hakemisto josta agentin profiili (SOUL, kalibrointi) ladataan.
    /// `None` = ei profiilia (paljas runko). Profiilin sisältö ei koskaan
    /// kuulu KERROS A:n repoon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_dir: Option<PathBuf>,
}

impl AgentConfig {
    /// Rakentaa agenttikonfiguraation nimellä ja mallilla, uusi satunnainen id.
    pub fn new(name: impl Into<String>, model: ModelConfig) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            model,
            profile_dir: None,
        }
    }

    /// Asettaa profiilihakemiston (builder-tyyli).
    #[must_use]
    pub fn with_profile_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.profile_dir = Some(dir.into());
        self
    }

    /// Validoi agenttikonfiguraation.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] jos nimi on tyhjä, malli kelvoton tai
    /// asetettu profiilipolku on tyhjä.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(FamilyClawError::config("agent name must not be empty"));
        }
        self.model.validate()?;
        if let Some(dir) = &self.profile_dir {
            if dir.as_os_str().is_empty() {
                return Err(FamilyClawError::config(
                    "agent profile_dir must not be empty when set",
                ));
            }
        }
        Ok(())
    }
}

/// Koko perheen (agenttiryhmän) konfiguraatio.
///
/// Tämä on alustan juurikonfiguraatio: ryhmän identiteetti, jäsenet ja
/// globaali fallback-ketju jota agentit voivat periä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyConfig {
    /// Perheen vakaa tunniste. Oletuksena uusi satunnainen jos puuttuu.
    #[serde(default)]
    pub id: FamilyId,

    /// Perheen näyttönimi (geneerinen).
    pub name: String,

    /// Perheen jäsenet.
    #[serde(default)]
    pub agents: Vec<AgentConfig>,

    /// Globaali fallback-malliketju jota agentit voivat käyttää viimeisenä
    /// oljenkortena oman ketjunsa jälkeen.
    #[serde(default)]
    pub global_fallbacks: Vec<String>,
}

impl FamilyConfig {
    /// Rakentaa tyhjän perhekonfiguraation annetulla nimellä.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: FamilyId::new(),
            name: name.into(),
            agents: Vec::new(),
            global_fallbacks: Vec::new(),
        }
    }

    /// Lisää agentin perheeseen (builder-tyyli).
    #[must_use]
    pub fn with_agent(mut self, agent: AgentConfig) -> Self {
        self.agents.push(agent);
        self
    }

    /// Lataa perhekonfiguraation JSON-merkkijonosta ja validoi sen.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] jos JSON on kelvotonta, tai
    /// [`FamilyClawError::Config`] jos validointi epäonnistuu.
    pub fn from_json_str(json: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Lataa perhekonfiguraation JSON-tiedostosta ja validoi sen.
    ///
    /// # Errors
    /// [`FamilyClawError::Io`] jos tiedostoa ei voi lukea,
    /// [`FamilyClawError::Serde`] jos JSON on kelvotonta, tai
    /// [`FamilyClawError::Config`] jos validointi epäonnistuu.
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_json_str(&contents)
    }

    /// Sarjallistaa konfiguraation siistiksi (pretty) JSON-merkkijonoksi.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] jos sarjallistus epäonnistuu.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(FamilyClawError::from)
    }

    /// Etsii agentin tunnisteen perusteella.
    #[must_use]
    pub fn agent_by_id(&self, id: AgentId) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Etsii agentin nimen perusteella.
    #[must_use]
    pub fn agent_by_name(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Validoi koko perhekonfiguraation rekursiivisesti.
    ///
    /// Tarkistaa: nimi ei tyhjä, kaikki agentit validit, agenttinimet ja
    /// -tunnisteet uniikkeja, globaalit fallbackit ei tyhjiä.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] kaikista validointivirheistä.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(FamilyClawError::config("family name must not be empty"));
        }
        for agent in &self.agents {
            agent.validate()?;
        }
        // Uniikit nimet.
        for (i, a) in self.agents.iter().enumerate() {
            if self.agents[i + 1..].iter().any(|b| b.name == a.name) {
                return Err(FamilyClawError::config(format!(
                    "duplicate agent name: {}",
                    a.name
                )));
            }
        }
        // Uniikit tunnisteet (paitsi nil-tunnisteet jätetään ajonaikaisen
        // täydennyksen varaan — kaksi nilliä ei silti sallita).
        for (i, a) in self.agents.iter().enumerate() {
            if self.agents[i + 1..].iter().any(|b| b.id == a.id) {
                return Err(FamilyClawError::config(format!(
                    "duplicate agent id: {}",
                    a.id
                )));
            }
        }
        if self.global_fallbacks.iter().any(|m| m.trim().is_empty()) {
            return Err(FamilyClawError::config(
                "global_fallbacks entries must not be empty",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_agent(name: &str) -> AgentConfig {
        AgentConfig::new(
            name,
            ModelConfig::new("provider/model").with_fallback("provider/backup"),
        )
    }

    #[test]
    fn model_preference_order_includes_primary_then_fallbacks() {
        let m = ModelConfig::new("a").with_fallback("b").with_fallback("c");
        let order: Vec<&str> = m.preference_order().collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn model_validate_rejects_empty_primary_and_fallback() {
        assert!(ModelConfig::new("  ").validate().is_err());
        assert!(ModelConfig::new("a").with_fallback("").validate().is_err());
        assert!(ModelConfig::new("a").with_fallback("b").validate().is_ok());
    }

    #[test]
    fn agent_validate_checks_name_model_and_profile() {
        assert!(sample_agent("agent_a").validate().is_ok());

        let mut bad_name = sample_agent("agent_a");
        bad_name.name = "   ".into();
        assert!(bad_name.validate().is_err());

        let bad_profile = sample_agent("agent_a").with_profile_dir(PathBuf::new());
        assert!(bad_profile.validate().is_err());

        let good_profile = sample_agent("agent_a").with_profile_dir("profiles/agent_a");
        assert!(good_profile.validate().is_ok());
    }

    #[test]
    fn family_builder_and_lookup() {
        let a = sample_agent("agent_a");
        let b = sample_agent("agent_b");
        let a_id = a.id;
        let family = FamilyConfig::new("test_family").with_agent(a).with_agent(b);

        assert_eq!(family.agents.len(), 2);
        assert_eq!(
            family.agent_by_id(a_id).map(|x| x.name.as_str()),
            Some("agent_a")
        );
        assert_eq!(
            family.agent_by_name("agent_b").map(|x| x.name.as_str()),
            Some("agent_b")
        );
        assert!(family.agent_by_name("missing").is_none());
        assert!(family.agent_by_id(AgentId::new()).is_none());
    }

    #[test]
    fn family_validate_detects_duplicate_names() {
        let family = FamilyConfig::new("f")
            .with_agent(sample_agent("dup"))
            .with_agent(sample_agent("dup"));
        let err = family.validate().expect_err("duplicate names rejected");
        assert!(err.to_string().contains("duplicate agent name"));
    }

    #[test]
    fn family_validate_detects_duplicate_ids() {
        let mut a = sample_agent("agent_a");
        let mut b = sample_agent("agent_b");
        let shared = AgentId::new();
        a.id = shared;
        b.id = shared;
        let family = FamilyConfig::new("f").with_agent(a).with_agent(b);
        let err = family.validate().expect_err("duplicate ids rejected");
        assert!(err.to_string().contains("duplicate agent id"));
    }

    #[test]
    fn family_validate_rejects_empty_name_and_bad_global_fallback() {
        assert!(FamilyConfig::new("   ").validate().is_err());

        let mut f = FamilyConfig::new("f");
        f.global_fallbacks.push(String::new());
        assert!(f.validate().is_err());
    }

    #[test]
    fn from_json_str_parses_minimal_and_applies_defaults() {
        let json = r#"{
            "name": "demo_family",
            "agents": [
                { "name": "agent_a", "model": { "primary": "provider/model" } }
            ]
        }"#;
        let family = FamilyConfig::from_json_str(json).expect("valid config parses");
        assert_eq!(family.name, "demo_family");
        assert_eq!(family.agents.len(), 1);
        let agent = &family.agents[0];
        assert_eq!(agent.name, "agent_a");
        assert!(agent.model.fallbacks.is_empty());
        // Oletukset täytettiin: id ei nil, ei profiilia.
        assert!(!agent.id.is_nil());
        assert!(agent.profile_dir.is_none());
        // Perheelle generoitui id.
        assert!(!family.id.is_nil());
    }

    #[test]
    fn from_json_str_rejects_invalid_config() {
        // Tyhjä primary → validointi kaatuu (ei serde).
        let json = r#"{
            "name": "f",
            "agents": [ { "name": "agent_a", "model": { "primary": "" } } ]
        }"#;
        let err = FamilyConfig::from_json_str(json).expect_err("invalid model rejected");
        assert!(matches!(err, FamilyClawError::Config(_)));
    }

    #[test]
    fn from_json_str_rejects_malformed_json() {
        let err = FamilyConfig::from_json_str("{ not json").expect_err("malformed rejected");
        assert!(matches!(err, FamilyClawError::Serde(_)));
    }

    #[test]
    fn json_roundtrip_preserves_config() {
        let family = FamilyConfig::new("roundtrip")
            .with_agent(sample_agent("agent_a").with_profile_dir("profiles/agent_a"));
        let json = family.to_json_string().expect("serialize");
        let back = FamilyConfig::from_json_str(&json).expect("deserialize");
        assert_eq!(family, back);
    }

    #[test]
    fn from_json_file_reads_and_validates() {
        let family = FamilyConfig::new("file_family").with_agent(sample_agent("agent_a"));
        let json = family.to_json_string().expect("serialize");

        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-core-test-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, &json).expect("write temp config");

        let loaded = FamilyConfig::from_json_file(&path).expect("load from file");
        assert_eq!(loaded, family);

        // Siivoa väliaikaistiedosto.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_json_file_missing_returns_io_error() {
        let err = FamilyConfig::from_json_file("definitely/not/here/family.json")
            .expect_err("missing file errors");
        assert!(matches!(err, FamilyClawError::Io(_)));
    }
}

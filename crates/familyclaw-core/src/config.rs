//! Configuration types for families and agents.
//!
//! Configuration is loaded from JSON (from a file or a string) and
//! validated. **Layer A / OSS boundary:** profiles (SOUL, calibration,
//! keys) are NOT part of this structure — agents reference an external
//! directory via the [`AgentConfig::profile_dir`] field (cf.
//! `FAMILYCLAW_PROFILE_DIR`). This file contains only the generic
//! structure, no hardcoded family/key/path data.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{FamilyClawError, Result};
use crate::ids::{AgentId, FamilyId};

/// LLM model configuration for a single agent.
///
/// Per-agent model plus global fallback chain (design §2.1, CORRECTIONS #5).
/// `primary` is the preferred model and `fallbacks` are backup models tried
/// in order. Filtering out underpowered models (too low TPM) is the
/// responsibility of the runtime layer — this type only carries the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// The preferred model (e.g. `"provider/model-name"`).
    pub primary: String,

    /// Backup models in order, tried if `primary` fails.
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

impl ModelConfig {
    /// Builds a model configuration with no fallbacks.
    pub fn new(primary: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            fallbacks: Vec::new(),
        }
    }

    /// Appends a fallback model to the end of the chain (builder style).
    #[must_use]
    pub fn with_fallback(mut self, model: impl Into<String>) -> Self {
        self.fallbacks.push(model.into());
        self
    }

    /// Iterates the full model preference order: `primary` first, then
    /// `fallbacks`.
    pub fn preference_order(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.primary.as_str()).chain(self.fallbacks.iter().map(String::as_str))
    }

    /// Validates the model configuration.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] if `primary` is empty or any fallback
    /// is an empty string.
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

/// Configuration for a single agent (family member).
///
/// Contains only the generic, publishable structure. The soul/persona is
/// loaded at runtime from the [`profile_dir`](AgentConfig::profile_dir)
/// directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// The agent's stable identifier. Defaults to a new random one if
    /// missing.
    #[serde(default)]
    pub id: AgentId,

    /// The agent's display name (generic, e.g. `"agent_a"`).
    pub name: String,

    /// Model configuration (primary + fallbacks).
    pub model: ModelConfig,

    /// Directory from which the agent's profile (SOUL, calibration) is
    /// loaded. `None` means no profile (bare runtime). Profile contents
    /// never belong in the Layer A repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_dir: Option<PathBuf>,
}

impl AgentConfig {
    /// Builds an agent configuration with a name and model, using a new
    /// random id.
    pub fn new(name: impl Into<String>, model: ModelConfig) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            model,
            profile_dir: None,
        }
    }

    /// Builds an agent configuration with a **stable** (name-derived)
    /// identifier and the given `model`.
    ///
    /// Unlike [`new`](Self::new), this does NOT assign a random id but
    /// derives it deterministically from the name ([`AgentId::from_name`]).
    /// This is the correct constructor for **production** use whenever the
    /// being's identity (and the `being_id` derived from it) must remain
    /// stable across process restarts — for example so that a resumable
    /// turn persisted on the crash-durable substrate matches the ownership
    /// check of the woken-up agent instead of being stuck unresumed
    /// forever.
    pub fn new_with_stable_id(name: impl Into<String>, model: ModelConfig) -> Self {
        let name = name.into();
        let id = AgentId::from_name(&name);
        Self {
            id,
            name,
            model,
            profile_dir: None,
        }
    }

    /// Sets the stable identifier explicitly (builder style).
    ///
    /// Used when the identifier is derived externally (e.g. from a
    /// profile) or needs to be pinned in tests. Leaves other fields
    /// unchanged.
    #[must_use]
    pub fn with_id(mut self, id: AgentId) -> Self {
        self.id = id;
        self
    }

    /// Sets the profile directory (builder style).
    #[must_use]
    pub fn with_profile_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.profile_dir = Some(dir.into());
        self
    }

    /// Validates the agent configuration.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] if the name is empty, the model is
    /// invalid, or a set profile path is empty.
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

/// Configuration for the whole family (agent group).
///
/// This is the platform's root configuration: the group's identity, its
/// members, and a global fallback chain that agents can inherit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyConfig {
    /// The family's stable identifier. Defaults to a new random one if
    /// missing.
    #[serde(default)]
    pub id: FamilyId,

    /// The family's display name (generic).
    pub name: String,

    /// The family's members.
    #[serde(default)]
    pub agents: Vec<AgentConfig>,

    /// Global fallback model chain that agents can use as a last resort
    /// after their own chain.
    #[serde(default)]
    pub global_fallbacks: Vec<String>,
}

impl FamilyConfig {
    /// Builds an empty family configuration with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: FamilyId::new(),
            name: name.into(),
            agents: Vec::new(),
            global_fallbacks: Vec::new(),
        }
    }

    /// Adds an agent to the family (builder style).
    #[must_use]
    pub fn with_agent(mut self, agent: AgentConfig) -> Self {
        self.agents.push(agent);
        self
    }

    /// Loads a family configuration from a JSON string and validates it.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] if the JSON is invalid, or
    /// [`FamilyClawError::Config`] if validation fails.
    pub fn from_json_str(json: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Loads a family configuration from a JSON file and validates it.
    ///
    /// # Errors
    /// [`FamilyClawError::Io`] if the file cannot be read,
    /// [`FamilyClawError::Serde`] if the JSON is invalid, or
    /// [`FamilyClawError::Config`] if validation fails.
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_json_str(&contents)
    }

    /// Serializes the configuration to a pretty-printed JSON string.
    ///
    /// # Errors
    /// [`FamilyClawError::Serde`] if serialization fails.
    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(FamilyClawError::from)
    }

    /// Looks up an agent by its identifier.
    #[must_use]
    pub fn agent_by_id(&self, id: AgentId) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Looks up an agent by its name.
    #[must_use]
    pub fn agent_by_name(&self, name: &str) -> Option<&AgentConfig> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// Recursively validates the whole family configuration.
    ///
    /// Checks: name not empty, all agents valid, agent names and
    /// identifiers unique, global fallbacks not empty.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] for any validation error.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(FamilyClawError::config("family name must not be empty"));
        }
        for agent in &self.agents {
            agent.validate()?;
        }
        // Unique names.
        for (i, a) in self.agents.iter().enumerate() {
            if self.agents[i + 1..].iter().any(|b| b.name == a.name) {
                return Err(FamilyClawError::config(format!(
                    "duplicate agent name: {}",
                    a.name
                )));
            }
        }
        // Unique identifiers (nil identifiers are left to be filled in at
        // runtime — two nils are still not allowed).
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
    fn new_with_stable_id_is_deterministic_and_distinct() {
        // STABILITY: building the same name twice → same id (simulates a restart).
        let a = AgentConfig::new_with_stable_id("agent_a", ModelConfig::new("provider/model"));
        let b = AgentConfig::new_with_stable_id("agent_a", ModelConfig::new("provider/model"));
        assert_eq!(a.id, b.id, "stable id survives a restart (same name)");
        assert_eq!(a.id, AgentId::from_name("agent_a"));
        // Different name → different id, so siblings don't share an identity.
        let other = AgentConfig::new_with_stable_id("operator", ModelConfig::new("provider/model"));
        assert_ne!(a.id, other.id);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn with_id_overrides_id_and_preserves_other_fields() {
        let fixed = AgentId::from_name("being_a");
        let cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model")).with_id(fixed);
        assert_eq!(cfg.id, fixed);
        assert_eq!(cfg.name, "agent_a");
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
        // Defaults were filled in: id not nil, no profile.
        assert!(!agent.id.is_nil());
        assert!(agent.profile_dir.is_none());
        // An id was generated for the family.
        assert!(!family.id.is_nil());
    }

    #[test]
    fn from_json_str_rejects_invalid_config() {
        // Empty primary → validation fails (not serde).
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

        // Clean up the temp file.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_json_file_missing_returns_io_error() {
        let err = FamilyConfig::from_json_file("definitely/not/here/family.json")
            .expect_err("missing file errors");
        assert!(matches!(err, FamilyClawError::Io(_)));
    }
}

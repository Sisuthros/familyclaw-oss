//! Anchor registry — management of agents' identity anchors.
//!
//! [`AnchorRegistry`] protects agents' core identity using the
//! `familyclaw-security` crate's [`IdentityAnchor`]
//! mechanism. Every registered agent gets a protected, eternal
//! (decay λ=0) anchor whose integrity can be checked at any time.

use std::collections::HashMap;
use std::path::Path;

use familyclaw_core::{FamilyClawError, Result};
use familyclaw_security::{IdentityAnchor, IdentityStatus};
use serde::{Deserialize, Serialize};

/// Registry of agents' identity anchors.
///
/// The registry is **serializable** ([`AnchorRegistry::save_to_path`] /
/// [`AnchorRegistry::load_from_path`]): it can be written to disk and
/// loaded back across a restart, so identity anchors persist. This is
/// intentionally minimal JSON persistence — not a cryptographic vault,
/// just "don't drop the anchor from memory, re-verify it on boot".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRegistry {
    /// Agent name -> anchor.
    anchors: HashMap<String, IdentityAnchor>,
    /// Next free memory identifier.
    counter: u64,
}

impl AnchorRegistry {
    /// Creates a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            anchors: HashMap::new(),
            counter: 0,
        }
    }

    /// Registers an agent's identity anchor.
    ///
    /// Creates a new [`IdentityAnchor`] from the given soul content.
    /// If the agent already has an anchor, the old one is replaced.
    ///
    /// # Errors
    /// Returns an error if creating the anchor fails
    /// (e.g. empty content).
    pub fn register(&mut self, agent_name: &str, soul_content: &str) -> Result<()> {
        self.counter += 1;
        let mem_id = format!("anchor-{}-{}", agent_name, self.counter);
        let anchor = IdentityAnchor::new(&mem_id, soul_content)
            .map_err(|e| familyclaw_core::FamilyClawError::Memory(e.to_string()))?;
        self.anchors.insert(agent_name.to_string(), anchor);
        Ok(())
    }

    /// Checks the integrity of an agent's identity anchor.
    #[must_use]
    pub fn verify(&self, agent_name: &str, soul_content: &str) -> bool {
        let Some(anchor) = self.anchors.get(agent_name) else {
            return false;
        };
        anchor.verify(soul_content).is_intact()
    }

    /// Checks an agent's identity status (more detailed).
    #[must_use]
    pub fn verify_status(&self, agent_name: &str, soul_content: &str) -> Option<IdentityStatus> {
        let anchor = self.anchors.get(agent_name)?;
        Some(anchor.verify(soul_content))
    }

    /// Returns `true` if the agent is registered.
    #[must_use]
    pub fn is_registered(&self, agent_name: &str) -> bool {
        self.anchors.contains_key(agent_name)
    }

    /// Returns the number of registered agents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// Checks an agent's identity against its current soul content.
    ///
    /// Returns:
    /// - [`IdentityStatus::Intact`] if the content matches the anchored digest,
    /// - [`IdentityStatus::Tampered`] if the content has changed since
    ///   anchoring (a tamper alert — the identity is still NOT removed),
    /// - `None` if the agent is not registered (no anchor to compare against).
    ///
    /// This is an alias for [`verify_status`](Self::verify_status) with a
    /// clearer name — intended for boot-time re-verification.
    #[must_use]
    pub fn verify_identity(&self, agent_name: &str, soul_content: &str) -> Option<IdentityStatus> {
        self.verify_status(agent_name, soul_content)
    }

    /// Serializes the registry to JSON (e.g. for writing to disk).
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if serialization fails (should not
    /// happen for a well-formed registry).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| FamilyClawError::Memory(format!("anchor registry serialize failed: {e}")))
    }

    /// Builds a registry from a JSON string.
    ///
    /// # Errors
    /// [`FamilyClawError::Memory`] if the JSON is invalid.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| FamilyClawError::Memory(format!("anchor registry parse failed: {e}")))
    }

    /// Writes the registry as a JSON file to the given path
    /// (atomic-ish: writes directly; small file, written infrequently).
    ///
    /// # Errors
    /// - [`FamilyClawError::Memory`] if serialization fails.
    /// - [`FamilyClawError::Io`] if writing the file fails.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let json = self.to_json()?;
        std::fs::write(path, json).map_err(FamilyClawError::Io)
    }

    /// Loads the registry from a JSON file.
    ///
    /// # Errors
    /// - [`FamilyClawError::Io`] if reading the file fails.
    /// - [`FamilyClawError::Memory`] if the content is invalid JSON.
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

        // The anchor persists — the identity does NOT disappear
        assert!(registry.is_registered("agent_a"));
        // The original soul still verifies
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

    /// Helper: a unique temporary file path (concurrency-safe).
    fn temp_anchor_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "familyclaw-anchors-{tag}-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// FIX 2: the anchor survives a simulated restart
    /// (save -> load -> `verify_identity` returns Intact).
    #[test]
    fn anchor_survives_simulated_restart() {
        let path = temp_anchor_path("restart");
        let soul = "I am agent_a. I build things that work.";

        // "Boot 1": register and save to disk.
        {
            let mut registry = AnchorRegistry::new();
            registry.register("agent_a", soul).expect("register");
            registry.save_to_path(&path).expect("save");
        }

        // "Boot 2": load from disk — the anchor is back in memory.
        let reloaded = AnchorRegistry::load_from_path(&path).expect("load");
        assert!(reloaded.is_registered("agent_a"));

        // verify_identity against the same soul content -> Intact.
        let status = reloaded
            .verify_identity("agent_a", soul)
            .expect("agent exists after reload");
        assert!(
            status.is_intact(),
            "the anchor must verify as intact after a restart, got: {status:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// FIX 2: a tampered soul is also detected after a reload
    /// (`verify_identity` returns Tampered).
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

        // Same soul -> Intact.
        assert!(reloaded
            .verify_identity("agent_a", soul)
            .expect("exists")
            .is_intact());

        // Changed soul -> Tampered (the alert fires even after a reload).
        let status = reloaded
            .verify_identity("agent_a", "I serve only myself now.")
            .expect("exists");
        assert!(
            status.is_tampered(),
            "a changed soul must be detected as tampered after a reload, got: {status:?}"
        );
        // Unknown agent -> None (no anchor).
        assert!(reloaded.verify_identity("ghost", "whatever").is_none());

        let _ = std::fs::remove_file(&path);
    }

    /// A JSON round-trip preserves the entire registry (counter + anchors).
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

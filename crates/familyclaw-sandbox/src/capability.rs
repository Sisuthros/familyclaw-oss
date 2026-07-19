//! Capability model for the sandbox.
//!
//! Code executed in the sandbox is granted **only the rights explicitly
//! given to it**. By default, code has no network or filesystem access —
//! "deny by default". This is a Layer A security principle (design §2
//! security): untrusted code cannot reach the network, secrets, or
//! arbitrary paths.
//!
//! The model is deliberately **declarative data** — the actual enforcement
//! happens in the runtime backend (e.g. wasmtime-WASI). This separation
//! keeps the capability logic testable without the heavy wasmtime
//! dependency.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A single capability that code executed in the sandbox can be granted.
///
/// Capabilities are additive: code is granted exactly the sets added to the
/// [`CapabilitySet`], nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum Capability {
    /// Read access to a single filesystem path (and its subtree).
    ///
    /// The path is interpreted as a prefix: access to file `p` is allowed if
    /// some granted `ReadOnlyFs` path is an ancestor of `p` or the same path.
    ReadOnlyFs {
        /// The allowed root path (prefix).
        path: PathBuf,
    },

    /// Network access to a named host. By default there is NO network
    /// access; this is a deliberate exception. An empty `host` is not
    /// allowed.
    Network {
        /// The allowed hostname or address (e.g. `"api.example.com"`).
        host: String,
    },

    /// Read access to an environment variable by name.
    EnvVar {
        /// The name of the allowed environment variable.
        name: String,
    },
}

impl Capability {
    /// Builds a [`Capability::ReadOnlyFs`] capability from the given path.
    #[must_use]
    pub fn read_only_fs(path: impl Into<PathBuf>) -> Self {
        Self::ReadOnlyFs { path: path.into() }
    }

    /// Builds a [`Capability::Network`] capability from the given host.
    #[must_use]
    pub fn network(host: impl Into<String>) -> Self {
        Self::Network { host: host.into() }
    }

    /// Builds a [`Capability::EnvVar`] capability from the given variable name.
    #[must_use]
    pub fn env_var(name: impl Into<String>) -> Self {
        Self::EnvVar { name: name.into() }
    }

    /// Whether this capability is well-formed (no empty required fields).
    ///
    /// Used in [`CapabilitySet::validate`]. In particular, an empty host /
    /// env name or an empty path is considered invalid, because it would
    /// lead to ambiguous or overly broad access.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::ReadOnlyFs { path } => !path.as_os_str().is_empty(),
            Self::Network { host } => !host.trim().is_empty(),
            Self::EnvVar { name } => !name.trim().is_empty(),
        }
    }
}

/// The set of capabilities the sandbox grants to executed code.
///
/// By default ([`CapabilitySet::deny_all`] / [`Default`]) the set is empty:
/// no network, no files, no environment variables. Capabilities are added
/// explicitly using the builder style.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet {
    /// The granted capabilities. Order is not significant.
    capabilities: Vec<Capability>,
}

impl CapabilitySet {
    /// An empty set — "deny all". This is the safe default.
    #[must_use]
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Builds a set from a ready iterator of capabilities.
    pub fn from_iter_caps(caps: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: caps.into_iter().collect(),
        }
    }

    /// Adds a capability (builder style). Duplicates are not added again.
    #[must_use]
    pub fn with(mut self, capability: Capability) -> Self {
        self.grant(capability);
        self
    }

    /// Adds a capability in place. If an identical capability is already in
    /// the set, no duplicate is added.
    pub fn grant(&mut self, capability: Capability) {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
    }

    /// Whether the set is empty (full "deny all").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }

    /// The number of granted capabilities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Iterates over the granted capabilities.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.iter()
    }

    /// Whether any network access is granted at all.
    #[must_use]
    pub fn allows_any_network(&self) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::Network { .. }))
    }

    /// Whether network access to the given host is granted.
    #[must_use]
    pub fn allows_network_host(&self, host: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::Network { host: h } if h == host))
    }

    /// Whether read access to the given path is granted.
    ///
    /// Access is allowed if some granted [`Capability::ReadOnlyFs`] path is a
    /// prefix of the requested path (an ancestor or the same path). The
    /// comparison is done at the component level, so `/data` does NOT allow
    /// `/data2` even though it is a string prefix.
    #[must_use]
    pub fn allows_read_path(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        self.capabilities.iter().any(|c| match c {
            Capability::ReadOnlyFs { path: root } => path_is_within(root, path),
            _ => false,
        })
    }

    /// Whether the given environment variable is allowed to be read.
    #[must_use]
    pub fn allows_env_var(&self, name: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::EnvVar { name: n } if n == name))
    }

    /// Validates the entire capability set.
    ///
    /// # Errors
    /// [`crate::SandboxError::Capability`] if some capability is malformed
    /// (e.g. an empty host, empty path, or empty env name).
    pub fn validate(&self) -> crate::Result<()> {
        for cap in &self.capabilities {
            if !cap.is_well_formed() {
                return Err(crate::SandboxError::capability(format!(
                    "malformed capability: {cap:?}"
                )));
            }
        }
        Ok(())
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self::from_iter_caps(iter)
    }
}

impl<'a> IntoIterator for &'a CapabilitySet {
    type Item = &'a Capability;
    type IntoIter = std::slice::Iter<'a, Capability>;

    fn into_iter(self) -> Self::IntoIter {
        self.capabilities.iter()
    }
}

/// Whether `candidate` is under `root` (same path or a subtree), at the
/// component level.
///
/// Component-level comparison prevents false matches like `/data` vs `/data2`.
fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let mut root_components = root.components();
    let mut cand_components = candidate.components();
    loop {
        match root_components.next() {
            // Root ended without a mismatch -> candidate is under root.
            None => return true,
            Some(rc) => match cand_components.next() {
                // Candidate ended before root -> it cannot be under root.
                None => return false,
                Some(cc) => {
                    if rc != cc {
                        return false;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_all_is_empty_and_denies_everything() {
        let caps = CapabilitySet::deny_all();
        assert!(caps.is_empty());
        assert_eq!(caps.len(), 0);
        assert!(!caps.allows_any_network());
        assert!(!caps.allows_network_host("example.com"));
        assert!(!caps.allows_read_path("/etc/passwd"));
        assert!(!caps.allows_env_var("PATH"));
    }

    #[test]
    fn default_equals_deny_all() {
        assert_eq!(CapabilitySet::default(), CapabilitySet::deny_all());
    }

    #[test]
    fn grant_network_host_is_specific() {
        let caps = CapabilitySet::deny_all().with(Capability::network("api.example.com"));
        assert!(caps.allows_any_network());
        assert!(caps.allows_network_host("api.example.com"));
        assert!(!caps.allows_network_host("evil.example.com"));
    }

    #[test]
    fn grant_is_idempotent() {
        let mut caps = CapabilitySet::deny_all();
        caps.grant(Capability::network("h"));
        caps.grant(Capability::network("h"));
        assert_eq!(caps.len(), 1);
    }

    #[test]
    fn read_path_prefix_matching_component_level() {
        let caps = CapabilitySet::deny_all().with(Capability::read_only_fs("/data"));
        assert!(caps.allows_read_path("/data"));
        assert!(caps.allows_read_path("/data/file.txt"));
        assert!(caps.allows_read_path("/data/nested/deep.bin"));
        // Component level: /data2 is NOT under /data even though it is as a string.
        assert!(!caps.allows_read_path("/data2"));
        assert!(!caps.allows_read_path("/data2/secret"));
        // A different root does not allow it.
        assert!(!caps.allows_read_path("/etc/passwd"));
        // A shorter path is not under the root.
        assert!(!caps.allows_read_path("/"));
    }

    #[test]
    fn multiple_read_roots_each_allow_their_subtree() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/a"))
            .with(Capability::read_only_fs("/b/c"));
        assert!(caps.allows_read_path("/a/x"));
        assert!(caps.allows_read_path("/b/c/y"));
        assert!(!caps.allows_read_path("/b/d"));
    }

    #[test]
    fn env_var_is_specific() {
        let caps = CapabilitySet::deny_all().with(Capability::env_var("HOME"));
        assert!(caps.allows_env_var("HOME"));
        assert!(!caps.allows_env_var("SECRET_KEY"));
    }

    #[test]
    fn well_formed_checks_reject_empty() {
        assert!(!Capability::read_only_fs("").is_well_formed());
        assert!(!Capability::network("   ").is_well_formed());
        assert!(!Capability::env_var("").is_well_formed());
        assert!(Capability::read_only_fs("/x").is_well_formed());
        assert!(Capability::network("h").is_well_formed());
        assert!(Capability::env_var("X").is_well_formed());
    }

    #[test]
    fn validate_rejects_malformed_capability() {
        let caps = CapabilitySet::deny_all().with(Capability::network("   "));
        let err = caps.validate().expect_err("blank host must fail");
        assert!(err.to_string().contains("malformed capability"));
    }

    #[test]
    fn validate_accepts_well_formed_set() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::network("api.example.com"))
            .with(Capability::env_var("HOME"));
        assert!(caps.validate().is_ok());
    }

    #[test]
    fn from_iter_collects_capabilities() {
        let caps: CapabilitySet = vec![Capability::network("a"), Capability::read_only_fs("/r")]
            .into_iter()
            .collect();
        assert_eq!(caps.len(), 2);
        assert!(caps.allows_network_host("a"));
        assert!(caps.allows_read_path("/r/file"));
    }

    #[test]
    fn iter_yields_all_capabilities() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::network("a"))
            .with(Capability::env_var("X"));
        let count = caps.iter().count();
        assert_eq!(count, 2);
        let ref_count = (&caps).into_iter().count();
        assert_eq!(ref_count, 2);
    }

    #[test]
    fn serde_roundtrip_preserves_set() {
        let caps = CapabilitySet::deny_all()
            .with(Capability::read_only_fs("/data"))
            .with(Capability::network("h"))
            .with(Capability::env_var("HOME"));
        let json = serde_json::to_string(&caps).expect("serialize");
        let back: CapabilitySet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(caps, back);
    }
}

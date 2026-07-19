//! Role-based access control ([`RbacPolicy`]) for agent capabilities.
//!
//! This is **defense in depth** on top of the wasmtime sandbox: while the
//! sandbox restricts what code *can* do, RBAC restricts what each role is
//! *permitted* to do. A policy maps a role ([`AgentRole`]) to the set of
//! allowed capability identifiers (e.g. `"browser"`, `"system.run"`).
//!
//! ## Principles
//! - **Deny by default.** An empty policy denies everything. Grants are
//!   added explicitly via the [`RbacPolicy::allow`] builder.
//! - **Deterministic.** A check is a pure function of the policy state.
//! - **OSS boundary:** roles and capabilities are generic identifiers, not
//!   private identities or secrets.

use std::collections::{HashMap, HashSet};

use familyclaw_bridge::AgentRole;
use familyclaw_core::{FamilyClawError, Result};

/// Error from an RBAC check.
///
/// A distinct type that converts into [`FamilyClawError`] (via the `?`
/// operator), so callers get a clear access-control error while still being
/// able to fold it into the platform's unified error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RbacError {
    /// The role does not have permission for the given capability.
    Denied {
        /// The role for which access was denied.
        role: AgentRole,
        /// The capability that was attempted.
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

/// Role-based access control policy.
///
/// Maps each [`AgentRole`] to a set of allowed capability identifiers.
/// By default (an empty policy) everything is denied; grants are added via
/// the [`allow`]-builder.
///
/// [`allow`]: RbacPolicy::allow
#[derive(Debug, Clone, Default)]
pub struct RbacPolicy {
    allowed: HashMap<AgentRole, HashSet<String>>,
}

impl RbacPolicy {
    /// Creates an empty policy (everything denied).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a grant: role `role` may use capability `capability`.
    ///
    /// Builder-style (returns `self`), so grants can be chained.
    /// Idempotent: adding the same grant twice does not change the state.
    #[must_use]
    pub fn allow(mut self, role: AgentRole, capability: impl Into<String>) -> Self {
        self.allowed
            .entry(role)
            .or_default()
            .insert(capability.into());
        self
    }

    /// Adds a grant in place (non-builder variant).
    pub fn grant(&mut self, role: AgentRole, capability: impl Into<String>) {
        self.allowed
            .entry(role)
            .or_default()
            .insert(capability.into());
    }

    /// Removes a grant. Returns `true` if the grant existed.
    pub fn revoke(&mut self, role: AgentRole, capability: &str) -> bool {
        self.allowed
            .get_mut(&role)
            .is_some_and(|set| set.remove(capability))
    }

    /// Whether the role has permission for the given capability (boolean check, no error).
    #[must_use]
    pub fn is_allowed(&self, role: AgentRole, capability: &str) -> bool {
        self.allowed
            .get(&role)
            .is_some_and(|set| set.contains(capability))
    }

    /// Checks permission and returns an error if access is denied.
    ///
    /// # Errors
    /// [`RbacError::Denied`] if the role does not have permission for the capability.
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

    /// Checks permission and converts the error into the platform's [`FamilyClawError`].
    ///
    /// A convenience method for when the caller is working with the
    /// [`Result`] type.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] (wrapping [`RbacError`]) if access
    /// is denied.
    pub fn check_core(&self, role: AgentRole, capability: &str) -> Result<()> {
        self.check(role, capability).map_err(FamilyClawError::from)
    }

    /// Returns the role's allowed capabilities in alphabetical order
    /// (deterministic, suitable for auditing/logging).
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

        // Another role does not inherit grants.
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
        // A second revoke of the same: did not exist.
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
        // A role with no grants -> empty.
        assert!(policy.capabilities_for(AgentRole::Scout).is_empty());
    }

    // --- Edge cases ---

    /// The empty string is a valid (but distinct) capability identifier:
    /// it must be granted explicitly, and it does not leak to other identifiers.
    #[test]
    fn empty_string_capability_is_distinct_and_must_be_granted() {
        // By default (not granted), the empty capability is denied.
        let denied = RbacPolicy::new();
        assert!(!denied.is_allowed(AgentRole::Executor, ""));
        assert!(denied.check(AgentRole::Executor, "").is_err());

        // An explicitly granted empty capability is allowed, but it does not
        // grant permission for a non-empty capability (nor vice versa).
        let policy = RbacPolicy::new().allow(AgentRole::Executor, "");
        assert!(policy.is_allowed(AgentRole::Executor, ""));
        assert!(policy.check(AgentRole::Executor, "").is_ok());
        assert!(!policy.is_allowed(AgentRole::Executor, "browser"));

        let only_named = RbacPolicy::new().allow(AgentRole::Executor, "browser");
        assert!(!only_named.is_allowed(AgentRole::Executor, ""));
    }

    /// Capabilities are matched with exact (case-sensitive) `HashSet` equality:
    /// `"Browser"` does not match a granted `"browser"`.
    #[test]
    fn capability_match_is_case_sensitive() {
        let policy = RbacPolicy::new().allow(AgentRole::Executor, "browser");

        // An exact match is allowed.
        assert!(policy.is_allowed(AgentRole::Executor, "browser"));

        // A different case does not match.
        assert!(!policy.is_allowed(AgentRole::Executor, "Browser"));
        assert!(!policy.is_allowed(AgentRole::Executor, "BROWSER"));
        assert!(policy.check(AgentRole::Executor, "Browser").is_err());

        // Revoke is also case-sensitive: the wrong case removes nothing.
        let mut mutable = RbacPolicy::new();
        mutable.grant(AgentRole::Executor, "browser");
        assert!(!mutable.revoke(AgentRole::Executor, "Browser"));
        assert!(mutable.is_allowed(AgentRole::Executor, "browser"));
        // With the correct case, revoke succeeds.
        assert!(mutable.revoke(AgentRole::Executor, "browser"));
    }
}

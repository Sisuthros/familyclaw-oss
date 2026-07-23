//! Operator ACL for gateway approval / audit surfaces.
//!
//! Complements [`crate::RbacPolicy`] (agent capabilities) with **human operator**
//! roles used by the HTTP control plane. Deny-by-default when enabled.

use familyclaw_core::{FamilyClawError, Result};

/// Human operator role on the gateway control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatorRole {
    /// Read pending approvals and audit only.
    Viewer,
    /// Viewer + approve/deny.
    Approver,
    /// Full control plane (tasks, audit, approvals).
    Admin,
}

impl OperatorRole {
    /// Parse a role name (case-insensitive). Unknown → `None`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Self::Viewer),
            "approver" => Some(Self::Approver),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// Stable wire name for headers / logs (never a secret).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Approver => "approver",
            Self::Admin => "admin",
        }
    }
}

/// Control-plane capability identifiers (see `docs/ENTERPRISE_AUTH.md`).
pub mod caps {
    /// List pending approvals.
    pub const APPROVALS_READ: &str = "approvals.read";
    /// Approve or deny.
    pub const APPROVALS_DECIDE: &str = "approvals.decide";
    /// Read turn audit.
    pub const AUDIT_READ: &str = "audit.read";
    /// Enable/disable scheduled tasks.
    pub const TASKS_CONTROL: &str = "tasks.control";
}

/// Deny-by-default operator ACL with the production default grants.
#[derive(Debug, Clone)]
pub struct OperatorAcl {
    enabled: bool,
}

impl OperatorAcl {
    /// ACL disabled → all checks succeed (bearer token remains the gate).
    #[must_use]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }

    /// ACL enabled with default role→capability map.
    #[must_use]
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    /// `FAMILYCLAW_OPERATOR_ACL=1` / `true` / `yes` → enabled.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var("FAMILYCLAW_OPERATOR_ACL") {
            Ok(v) => {
                let t = v.trim().to_ascii_lowercase();
                if t == "1" || t == "true" || t == "yes" {
                    Self::enabled()
                } else {
                    Self::disabled()
                }
            }
            Err(_) => Self::disabled(),
        }
    }

    /// Whether the ACL is enforcing role checks.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns whether `role` may use `capability`.
    #[must_use]
    pub fn allows(&self, role: OperatorRole, capability: &str) -> bool {
        if !self.enabled {
            return true;
        }
        match role {
            OperatorRole::Viewer => {
                matches!(capability, caps::APPROVALS_READ | caps::AUDIT_READ)
            }
            OperatorRole::Approver => matches!(
                capability,
                caps::APPROVALS_READ | caps::APPROVALS_DECIDE | caps::AUDIT_READ
            ),
            OperatorRole::Admin => matches!(
                capability,
                caps::APPROVALS_READ
                    | caps::APPROVALS_DECIDE
                    | caps::AUDIT_READ
                    | caps::TASKS_CONTROL
            ),
        }
    }

    /// Fallible check → [`FamilyClawError::invalid_input`] on deny.
    pub fn check(&self, role: OperatorRole, capability: &str) -> Result<()> {
        if self.allows(role, capability) {
            Ok(())
        } else {
            Err(FamilyClawError::invalid_input(format!(
                "operator acl: role '{}' denied capability '{capability}'",
                role.as_str()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_allows_everything() {
        let acl = OperatorAcl::disabled();
        assert!(acl.allows(OperatorRole::Viewer, caps::APPROVALS_DECIDE));
    }

    #[test]
    fn viewer_cannot_decide() {
        let acl = OperatorAcl::enabled();
        assert!(acl.allows(OperatorRole::Viewer, caps::APPROVALS_READ));
        assert!(!acl.allows(OperatorRole::Viewer, caps::APPROVALS_DECIDE));
        assert!(acl
            .check(OperatorRole::Viewer, caps::APPROVALS_DECIDE)
            .is_err());
    }

    #[test]
    fn approver_and_admin_grants() {
        let acl = OperatorAcl::enabled();
        assert!(acl.allows(OperatorRole::Approver, caps::APPROVALS_DECIDE));
        assert!(acl.allows(OperatorRole::Admin, caps::TASKS_CONTROL));
        assert!(!acl.allows(OperatorRole::Approver, caps::TASKS_CONTROL));
    }

    #[test]
    fn parse_roles() {
        assert_eq!(
            OperatorRole::parse("Approver"),
            Some(OperatorRole::Approver)
        );
        assert_eq!(OperatorRole::parse("nope"), None);
    }
}

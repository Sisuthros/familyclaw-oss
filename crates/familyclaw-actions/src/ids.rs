//! Typed identifiers (newtype) for the action and proof stack.
//!
//! Same design principle as `familyclaw-core::ids`: each identifier wraps a
//! [`uuid::Uuid`] value in its own type, so the compiler prevents mixing up
//! different identifier types (e.g. so a [`SkillId`] value can't
//! accidentally be passed where an [`ActionTaskId`] is expected).
//!
//! All identifiers:
//! - serialize to the same form as the underlying UUID (`serde transparent`),
//! - support `v4` random generation via a [`SkillId::new`]-style constructor,
//! - parse from a string via the [`std::str::FromStr`] implementation,
//! - print as a canonical UUID string via the [`std::fmt::Display`] implementation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generates a newtype identifier type with the given name and documentation.
///
/// The macro keeps the implementations identical across all identifiers and
/// reduces repetition without changing the public API.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new random (`v4`) identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing [`Uuid`] value into this identifier type.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the inner [`Uuid`] value.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Consumes the identifier and returns the inner [`Uuid`] value.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// The `nil` identifier (all zeros) — used as a default/empty value.
            #[must_use]
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }

            /// Whether this is the `nil` identifier.
            #[must_use]
            pub fn is_nil(&self) -> bool {
                self.0.is_nil()
            }
        }

        impl Default for $name {
            /// Defaults to a new random identifier — so entities always get
            /// a unique identity without a separate call.
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

define_id! {
    /// Identifier for a single skill in the registry.
    SkillId
}

define_id! {
    /// Identifier for an executable action task.
    ///
    /// Distinct from `familyclaw-bridge` task identifiers: this refers to a
    /// task in the action stack (observe→…→report), not the orchestration table.
    ActionTaskId
}

define_id! {
    /// Identifier for an approval request (human-in-the-loop).
    ApprovalId
}

define_id! {
    /// Identifier for a proof bundle.
    ProofBundleId
}

define_id! {
    /// Identifier for a single executed action.
    ActionId
}

define_id! {
    /// Identifier for a single audit-log event.
    AuditEventId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique_and_nonnil() {
        let a = SkillId::new();
        let b = SkillId::new();
        assert_ne!(a, b);
        assert!(!a.is_nil());
    }

    #[test]
    fn nil_is_nil() {
        assert!(SkillId::nil().is_nil());
        assert!(ActionTaskId::nil().is_nil());
        assert!(ApprovalId::nil().is_nil());
        assert!(ProofBundleId::nil().is_nil());
        assert!(ActionId::nil().is_nil());
        assert!(AuditEventId::nil().is_nil());
    }

    #[test]
    fn display_and_fromstr_roundtrip() {
        let id = ApprovalId::new();
        let text = id.to_string();
        let parsed: ApprovalId = text.parse().expect("valid uuid string parses");
        assert_eq!(id, parsed);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        assert!("not-a-uuid".parse::<ActionId>().is_err());
    }

    #[test]
    fn uuid_conversions_roundtrip() {
        let raw = Uuid::new_v4();
        let id = ProofBundleId::from_uuid(raw);
        assert_eq!(id.as_uuid(), &raw);
        assert_eq!(id.into_uuid(), raw);
    }

    #[test]
    fn serde_is_transparent_to_uuid() {
        let id = AuditEventId::new();
        let id_json = serde_json::to_string(&id).expect("serialize id");
        let uuid_json = serde_json::to_string(id.as_uuid()).expect("serialize uuid");
        assert_eq!(id_json, uuid_json);

        let back: AuditEventId = serde_json::from_str(&id_json).expect("deserialize id");
        assert_eq!(back, id);
    }
}

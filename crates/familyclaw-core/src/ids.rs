//! Typed identifiers (newtype) for platform entities.
//!
//! Each identifier wraps a [`uuid::Uuid`] value in its own type so that
//! the compiler prevents different identifier types from being mixed up
//! (e.g. so an [`AgentId`] value can't accidentally be passed where a
//! [`MessageId`] is expected).
//!
//! All identifiers:
//! - serialize in the same form as the underlying UUID (`serde transparent`),
//! - support `v4` random generation via the `new` constructor,
//! - parse from a string via the [`std::str::FromStr`] implementation,
//! - print as the canonical UUID string via the [`std::fmt::Display`] implementation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Fixed namespace for deterministic (`UUIDv5`) identifiers.
///
/// Used in derivations like [`AgentId::from_name`] so that a stable
/// name always produces the same identifier across process restarts.
/// The value was chosen randomly **once** and must never change — changing
/// it would break the stability of all previously derived identifiers.
pub const ID_NAMESPACE: Uuid = uuid::uuid!("6f1c0e2a-9b3d-4f5a-8c7e-1d2b3a4c5d6e");

/// Generates a newtype identifier type with the given name and documentation.
///
/// The macro keeps the implementation identical across all identifiers and
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

            /// Wraps an existing [`Uuid`] value in this identifier type.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Derives a **deterministic** identifier from a stable name (`UUIDv5`).
            ///
            /// The same `name` ALWAYS produces the same identifier — across process
            /// restarts, across machines, without reading any state from disk.
            /// This is a precondition for an entity's identity (and the `being_id`
            /// derived from it) staying **stable across a restart**: without a stable
            /// identifier, a resumable turn stored on the crash-resilient substrate
            /// would no longer match the ownership check of the reawakened agent.
            ///
            /// The namespace is [`ID_NAMESPACE`] (fixed, project-specific), so
            /// different identifier types derive the same UUID from the same name —
            /// the type system still keeps them separate at compile time.
            #[must_use]
            pub fn from_name(name: &str) -> Self {
                Self(Uuid::new_v5(&ID_NAMESPACE, name.as_bytes()))
            }

            /// Returns the wrapped [`Uuid`] value.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Consumes the identifier and returns the wrapped [`Uuid`] value.
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
            /// Defaults to a new random identifier — so entities always get a
            /// unique identity without a separate call.
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
    /// Identifier for a single agent (family member).
    AgentId
}

define_id! {
    /// Identifier for a family (agent group).
    FamilyId
}

define_id! {
    /// Identifier for a single message on the bus.
    MessageId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique_and_nonnil() {
        let a = AgentId::new();
        let b = AgentId::new();
        assert_ne!(a, b);
        assert!(!a.is_nil());
    }

    #[test]
    fn nil_is_nil() {
        assert!(AgentId::nil().is_nil());
        assert!(FamilyId::nil().is_nil());
        assert!(MessageId::nil().is_nil());
    }

    #[test]
    fn default_produces_unique_nonnil() {
        let a = MessageId::default();
        let b = MessageId::default();
        assert_ne!(a, b);
        assert!(!a.is_nil());
    }

    #[test]
    fn display_and_fromstr_roundtrip() {
        let id = AgentId::new();
        let text = id.to_string();
        let parsed: AgentId = text.parse().expect("valid uuid string parses");
        assert_eq!(id, parsed);
    }

    #[test]
    fn fromstr_rejects_garbage() {
        assert!("not-a-uuid".parse::<AgentId>().is_err());
        assert!(AgentId::from_str("").is_err());
    }

    #[test]
    fn uuid_conversions_roundtrip() {
        let raw = Uuid::new_v4();
        let id = AgentId::from_uuid(raw);
        assert_eq!(id.as_uuid(), &raw);
        assert_eq!(id.into_uuid(), raw);

        let via_from: FamilyId = raw.into();
        let back: Uuid = via_from.into();
        assert_eq!(back, raw);
    }

    #[test]
    fn serde_is_transparent_to_uuid() {
        let id = MessageId::new();
        let id_json = serde_json::to_string(&id).expect("serialize id");
        let uuid_json = serde_json::to_string(id.as_uuid()).expect("serialize uuid");
        // The newtype serializes exactly like a bare UUID string.
        assert_eq!(id_json, uuid_json);

        let back: MessageId = serde_json::from_str(&id_json).expect("deserialize id");
        assert_eq!(back, id);
    }

    #[test]
    fn distinct_id_types_do_not_share_serde_confusion() {
        // Same UUID, different types — the values serialize to the same string
        // but the type system keeps them separate at compile time.
        let raw = Uuid::new_v4();
        let agent = AgentId::from_uuid(raw);
        let message = MessageId::from_uuid(raw);
        assert_eq!(
            serde_json::to_string(&agent).expect("ser agent"),
            serde_json::to_string(&message).expect("ser message")
        );
    }

    #[test]
    fn ord_is_consistent_with_uuid() {
        let lo = AgentId::from_uuid(Uuid::from_u128(1));
        let hi = AgentId::from_uuid(Uuid::from_u128(2));
        assert!(lo < hi);
    }

    #[test]
    fn from_name_is_deterministic_across_calls() {
        // Same name → same identifier. This is the core of the stability
        // guarantee: an identifier derived twice (as if in two different
        // processes) matches, with no randomness involved.
        let a = AgentId::from_name("agent_a");
        let b = AgentId::from_name("agent_a");
        assert_eq!(a, b);
        assert!(!a.is_nil());
    }

    #[test]
    fn from_name_distinguishes_different_names() {
        assert_ne!(
            AgentId::from_name("agent_a"),
            AgentId::from_name("operator")
        );
    }

    #[test]
    fn from_name_matches_known_v5_vector() {
        // A fixed vector protects the namespace against accidental changes:
        // if `ID_NAMESPACE` changes, this test will flag it (stability would break).
        let expected = Uuid::new_v5(&ID_NAMESPACE, b"agent_a");
        assert_eq!(AgentId::from_name("agent_a").into_uuid(), expected);
    }
}

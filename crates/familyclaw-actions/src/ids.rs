//! Tyypitetyt tunnisteet (newtype) toiminto- ja todistepinolle.
//!
//! Sama suunnitteluperiaate kuin `familyclaw-core::ids`: jokainen tunniste
//! kääri [`uuid::Uuid`]-arvon omaan tyyppiinsä, jotta kääntäjä estää eri
//! tunnistetyyppien sekoittamisen (esim. ettei [`SkillId`]-arvoa voi
//! vahingossa antaa [`ActionTaskId`]-paikkaan).
//!
//! Kaikki tunnisteet:
//! - sarjallistuvat samaan muotoon kuin alla oleva UUID (`serde transparent`),
//! - tukevat `v4`-satunnaisgenerointia [`SkillId::new`]-tyylisellä konstruktorilla,
//! - jäsentyvät merkkijonosta [`std::str::FromStr`]-toteutuksella,
//! - tulostuvat kanonisena UUID-merkkijonona [`std::fmt::Display`]-toteutuksella.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generoi newtype-tunnistetyypin annetulla nimellä ja dokumentaatiolla.
///
/// Makro pitää toteutukset identtisinä kaikille tunnisteille ja vähentää
/// toistoa ilman että julkinen API muuttuu.
macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Luo uuden satunnaisen (`v4`) tunnisteen.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Kääri olemassa olevan [`Uuid`]-arvon tähän tunnistetyyppiin.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Palauttaa sisällä olevan [`Uuid`]-arvon.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Kuluttaa tunnisteen ja palauttaa sisällä olevan [`Uuid`]-arvon.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// `nil`-tunniste (kaikki nollia) — käytetään oletus-/tyhjäarvona.
            #[must_use]
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }

            /// Onko tämä `nil`-tunniste.
            #[must_use]
            pub fn is_nil(&self) -> bool {
                self.0.is_nil()
            }
        }

        impl Default for $name {
            /// Oletuksena uusi satunnainen tunniste — jotta entiteetit
            /// saavat aina ainutkertaisen identiteetin ilman erillistä kutsua.
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
    /// Yksittäisen taidon (skill) tunniste rekisterissä.
    SkillId
}

define_id! {
    /// Suoritettavan toimintotehtävän tunniste (action-task).
    ///
    /// Erillinen `familyclaw-bridge`-tehtävien tunnisteesta: tämä viittaa
    /// toimintopinon (observe→…→report) tehtävään, ei orkesteroinnin tauluun.
    ActionTaskId
}

define_id! {
    /// Hyväksyntäpyynnön (human-in-the-loop) tunniste.
    ApprovalId
}

define_id! {
    /// Todistepaketin (proof bundle) tunniste.
    ProofBundleId
}

define_id! {
    /// Yksittäisen suoritetun toiminnon tunniste.
    ActionId
}

define_id! {
    /// Audit-lokin yksittäisen tapahtuman tunniste.
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

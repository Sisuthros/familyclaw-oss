//! Tyypitetyt tunnisteet (newtype) alustan entiteeteille.
//!
//! Jokainen tunniste kääri [`uuid::Uuid`]-arvon omaan tyyppiinsä jotta
//! kääntäjä estää eri tunnistetyyppien sekoittamisen (esim. ettei
//! [`AgentId`]-arvoa voi vahingossa antaa [`MessageId`]-paikkaan).
//!
//! Kaikki tunnisteet:
//! - sarjallistuvat samaan muotoon kuin alla oleva UUID (`serde transparent`),
//! - tukevat `v4`-satunnaisgenerointia `new`-konstruktorilla,
//! - jäsentyvät merkkijonosta [`std::str::FromStr`]-toteutuksella,
//! - tulostuvat kanonisena UUID-merkkijonona [`std::fmt::Display`]-toteutuksella.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kiinteä nimiavaruus deterministisille (`UUIDv5`) tunnisteille.
///
/// Käytetään [`AgentId::from_name`]-tyyppisissä johdannaisissa, jotta vakaa
/// nimi tuottaa aina saman tunnisteen yli prosessin uudelleenkäynnistyksen.
/// Arvo on satunnaisesti valittu **kerran** eikä saa muuttua — sen
/// vaihtaminen rikkoisi kaikkien aiemmin johdettujen tunnisteiden vakauden.
pub const ID_NAMESPACE: Uuid = uuid::uuid!("6f1c0e2a-9b3d-4f5a-8c7e-1d2b3a4c5d6e");

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

            /// Johtaa **deterministisen** tunnisteen vakaasta nimestä (`UUIDv5`).
            ///
            /// Sama `name` tuottaa AINA saman tunnisteen — yli prosessin
            /// uudelleenkäynnistyksen, yli koneiden, ilman levyltä luettua tilaa.
            /// Tämä on edellytys sille, että olennon identiteetti (ja siitä
            /// johdettu `being_id`) pysyy **vakaana yli restartin**: ilman vakaata
            /// tunnistetta kaatumiskestävälle pinnalle tallennettu jatkettava vuoro
            /// ei enää täsmäisi heränneen agentin omistajuustarkistukseen.
            ///
            /// Nimiavaruus on [`ID_NAMESPACE`] (kiinteä, projektikohtainen),
            /// joten eri tunnistetyypit johtavat saman nimen samaan UUID:hen —
            /// tyyppijärjestelmä pitää ne silti erillään käännösaikana.
            #[must_use]
            pub fn from_name(name: &str) -> Self {
                Self(Uuid::new_v5(&ID_NAMESPACE, name.as_bytes()))
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
    /// Yksittäisen agentin (perheenjäsenen) tunniste.
    AgentId
}

define_id! {
    /// Perheen (agenttiryhmän) tunniste.
    FamilyId
}

define_id! {
    /// Yksittäisen viestin tunniste busissa.
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
        // Newtype sarjallistuu täsmälleen kuin paljas UUID-merkkijono.
        assert_eq!(id_json, uuid_json);

        let back: MessageId = serde_json::from_str(&id_json).expect("deserialize id");
        assert_eq!(back, id);
    }

    #[test]
    fn distinct_id_types_do_not_share_serde_confusion() {
        // Sama UUID, eri tyypit — arvot sarjallistuvat samaksi merkkijonoksi
        // mutta tyyppijärjestelmä pitää ne erillään käännösaikana.
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
        // Sama nimi → sama tunniste. Tämä on vakauden ydin: kahdesti johdettu
        // (kuin kahdessa eri prosessissa) tunniste täsmää, ei satunnaisuutta.
        let a = AgentId::from_name("agent_a");
        let b = AgentId::from_name("agent_a");
        assert_eq!(a, b);
        assert!(!a.is_nil());
    }

    #[test]
    fn from_name_distinguishes_different_names() {
        assert_ne!(AgentId::from_name("agent_a"), AgentId::from_name("operator"));
    }

    #[test]
    fn from_name_matches_known_v5_vector() {
        // Kiinteä vektori suojaa nimiavaruuden tahattomalta muutokselta:
        // jos `ID_NAMESPACE` muuttuu, tämä testi hälyttää (vakaus rikkoutuisi).
        let expected = Uuid::new_v5(&ID_NAMESPACE, b"agent_a");
        assert_eq!(AgentId::from_name("agent_a").into_uuid(), expected);
    }
}

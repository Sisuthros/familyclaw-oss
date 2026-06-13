//! Muiston **alkuperä** ([`Provenance`]) ja myrkytyssuoja ([`ProvenanceGate`]).
//!
//! Eternal Threadin muisti on hyökkäyspinta: *Sleeper Memory Poisoning*
//! (arXiv 2605.15338) raportoi 99.8 % injektio-onnistumisen kun muistilla ei
//! ole alkuperätietoa, ja *`MemPoison`* (arXiv 2605.29960) ohittaa valikoivan
//! muistin. Eywa-periaate vastaa: **"evidence before belief"** — immutaabelit
//! lähteet ([`Provenance::DirectExperience`]) → johdetut faktat
//! ([`Provenance::Derived`]) → ulkoiset, luottamuksella punnitut väitteet
//! ([`Provenance::External`]).
//!
//! [`ProvenanceGate`] on porttivahti: se hylkää matalan luottamuksen ulkoisen
//! lähteen ennen kuin se pääsee muistiin. Suora kokemus ja johdetut muistot
//! pääsevät aina läpi — vain ulkoiset väitteet punnitaan luottamuskynnystä
//! vasten.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_memory::{Provenance, ProvenanceGate};
//!
//! let gate = ProvenanceGate::new(0.6);
//!
//! // Suora kokemus pääsee aina.
//! assert!(gate.admit(&Provenance::DirectExperience));
//!
//! // Luotettu ulkoinen lähde pääsee.
//! assert!(gate.admit(&Provenance::external("web", 0.9)));
//!
//! // Matalan luottamuksen ulkoinen lähde hylätään (myrkytyssuoja).
//! assert!(!gate.admit(&Provenance::external("web", 0.1)));
//! ```

use familyclaw_core::MessageId;
use serde::{Deserialize, Serialize};

/// Muiston alkuperä — mistä tämä tieto on peräisin ja kuinka luotettava se on.
///
/// Alkuperä järjestää muistot luottamushierarkiaan:
/// 1. [`DirectExperience`](Provenance::DirectExperience) — olennon oma havainto
///    (korkein luottamus, ei punnita).
/// 2. [`Derived`](Provenance::Derived) — johdettu olemassa olevista muistoista
///    (esim. reflektio, yhdistely); periytyy lähteidensä luotettavuudesta.
/// 3. [`External`](Provenance::External) — ulkopuolinen lähde (esim. `"web"`,
///    `"tool"`) eksplisiittisellä luottamuksella `0.0..=1.0` — punnitaan
///    [`ProvenanceGate`]illa ennen muistiin pääsyä.
///
/// Oletus on [`DirectExperience`](Provenance::DirectExperience): vanhat,
/// ennen alkuperätietoa persistoidut muistot tulkitaan suoraksi kokemukseksi
/// (taaksepäin-yhteensopiva serde-default [`Memory`](crate::Memory)issa).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// Olennon oma suora havainto. Korkein luottamus — ei koskaan hylätä.
    DirectExperience,

    /// Johdettu olemassa olevista muistoista (reflektio, yhdistely).
    ///
    /// `from` viittaa lähde­muistojen tunnisteisiin, jotta johdannan ketju
    /// säilyy auditoitavana (Eywa: johdetut faktat osoittavat lähteisiinsä).
    Derived {
        /// Lähde­muistojen tunnisteet joista tämä on johdettu.
        from: Vec<MessageId>,
    },

    /// Ulkopuolinen lähde eksplisiittisellä luottamuksella.
    ///
    /// `source` on geneerinen lähde­tunniste (esim. `"web"`, `"tool"`,
    /// `"doc"`); `trust` on `0.0..=1.0` luottamus jota
    /// [`ProvenanceGate`] punnitsee. Matala `trust` = mahdollinen
    /// myrkytys → hylätään portilla.
    External {
        /// Geneerinen lähde­tunniste (esim. `"web"`).
        source: String,
        /// Luottamus lähteeseen, `0.0..=1.0`.
        trust: f32,
    },
}

impl Provenance {
    /// Rakentaa [`External`](Provenance::External)-alkuperän; `trust`
    /// puristetaan välille `0.0..=1.0` (ei-äärelliset arvot → `0.0`).
    #[must_use]
    pub fn external(source: impl Into<String>, trust: f32) -> Self {
        Self::External {
            source: source.into(),
            trust: clamp_trust(trust),
        }
    }

    /// Rakentaa [`Derived`](Provenance::Derived)-alkuperän annetuista
    /// lähde­tunnisteista.
    #[must_use]
    pub fn derived(from: impl IntoIterator<Item = MessageId>) -> Self {
        Self::Derived {
            from: from.into_iter().collect(),
        }
    }

    /// Alkuperän efektiivinen luottamuskerroin `0.0..=1.0`
    /// retrieval-painotusta varten.
    ///
    /// - [`DirectExperience`](Provenance::DirectExperience) → `1.0`
    ///   (oma havainto, täysi luottamus).
    /// - [`Derived`](Provenance::Derived) → `1.0` (johdettu jo
    ///   hyväksytyistä muistoista; lähteiden punninta tapahtui
    ///   kirjaushetkellä).
    /// - [`External`](Provenance::External) → `trust` (punnittu
    ///   `0.0..=1.0`).
    #[must_use]
    pub fn trust(&self) -> f32 {
        match self {
            Self::DirectExperience | Self::Derived { .. } => 1.0,
            Self::External { trust, .. } => clamp_trust(*trust),
        }
    }

    /// Onko alkuperä ulkopuolinen (eli portin punnittava)?
    #[must_use]
    pub const fn is_external(&self) -> bool {
        matches!(self, Self::External { .. })
    }
}

impl Default for Provenance {
    /// Oletus on suora kokemus — vanhat muistot ilman alkuperätietoa
    /// tulkitaan luotetuksi omaksi havainnoksi.
    fn default() -> Self {
        Self::DirectExperience
    }
}

/// Puristaa luottamuksen `0.0..=1.0`; ei-äärelliset arvot (NaN, ±∞) → `0.0`
/// (turvallinen oletus: tuntematon luottamus = ei luottamusta).
fn clamp_trust(trust: f32) -> f32 {
    if trust.is_finite() {
        trust.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Alkuperä-portti: myrkytyssuoja joka hylkää matalan luottamuksen ulkoiset
/// lähteet ennen kuin ne pääsevät muistiin.
///
/// Suora kokemus ja johdetut muistot pääsevät **aina** läpi (niiden luottamus
/// on `1.0`). Vain [`Provenance::External`] punnitaan: jos sen `trust` alittaa
/// [`min_trust`](ProvenanceGate::min_trust), [`admit`](ProvenanceGate::admit)
/// palauttaa `false` ja kutsujan tulee hylätä muisto (Sleeper Memory
/// Poisoning -suoja).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceGate {
    /// Pienin hyväksyttävä luottamus ulkoiselle lähteelle, `0.0..=1.0`.
    min_trust: f32,
}

impl ProvenanceGate {
    /// Luo portin annetulla luottamuskynnyksellä; `min_trust` puristetaan
    /// välille `0.0..=1.0` (ei-äärelliset arvot → `0.0` = päästä kaikki).
    #[must_use]
    pub fn new(min_trust: f32) -> Self {
        Self {
            min_trust: clamp_trust(min_trust),
        }
    }

    /// Portin luottamuskynnys (`0.0..=1.0`).
    #[must_use]
    pub const fn min_trust(&self) -> f32 {
        self.min_trust
    }

    /// Hyväksytäänkö annettu alkuperä muistiin?
    ///
    /// - Suora kokemus ja johdetut muistot → aina `true`.
    /// - Ulkoinen lähde → `true` vain jos `trust >= min_trust`.
    ///
    /// Matalan luottamuksen ulkoinen väite hylätään (`false`) — tämä on
    /// myrkytyssuoja: hyökkääjän syöttämä epäluotettava "fakta" ei pääse
    /// muistiin saastuttamaan myöhempää haetua.
    #[must_use]
    pub fn admit(&self, provenance: &Provenance) -> bool {
        match provenance {
            Provenance::DirectExperience | Provenance::Derived { .. } => true,
            Provenance::External { trust, .. } => clamp_trust(*trust) >= self.min_trust,
        }
    }
}

impl Default for ProvenanceGate {
    /// Maltillinen oletuskynnys (`0.5`): ulkoinen lähde tarvitsee vähintään
    /// keskinkertaisen luottamuksen päästäkseen muistiin.
    fn default() -> Self {
        Self::new(0.5)
    }
}

#[cfg(test)]
mod tests {
    // Osa testeistä vertaa tarkkoja f32-vakioita — tarkka vertailu on ok.
    #![allow(clippy::float_cmp)]

    use super::*;
    use familyclaw_core::MessageId;

    #[test]
    fn gate_admits_direct_experience() {
        let gate = ProvenanceGate::new(0.9);
        // Suora kokemus pääsee vaikka kynnys on korkea.
        assert!(gate.admit(&Provenance::DirectExperience));
    }

    #[test]
    fn gate_admits_derived_chain() {
        let gate = ProvenanceGate::new(0.99);
        let sources = vec![MessageId::new(), MessageId::new()];
        let derived = Provenance::derived(sources.clone());
        // Johdettu pääsee aina; lähde­ketju säilyy auditoitavana.
        assert!(gate.admit(&derived));
        match derived {
            Provenance::Derived { from } => assert_eq!(from, sources),
            other => panic!("odotettiin Derived, saatiin {other:?}"),
        }
    }

    #[test]
    fn gate_rejects_low_trust_external() {
        let gate = ProvenanceGate::new(0.6);
        // Matalan luottamuksen ulkoinen lähde hylätään (myrkytyssuoja).
        assert!(!gate.admit(&Provenance::external("web", 0.1)));
    }

    #[test]
    fn gate_admits_high_trust_external() {
        let gate = ProvenanceGate::new(0.6);
        // Riittävän luotettu ulkoinen lähde pääsee.
        assert!(gate.admit(&Provenance::external("web", 0.9)));
    }

    #[test]
    fn gate_boundary_is_inclusive() {
        let gate = ProvenanceGate::new(0.5);
        // Täsmälleen kynnyksellä → hyväksytään (>=).
        assert!(gate.admit(&Provenance::external("tool", 0.5)));
        // Aavistuksen alle → hylätään.
        assert!(!gate.admit(&Provenance::external("tool", 0.4999)));
    }

    #[test]
    fn external_trust_is_clamped() {
        // Yli rajan puristuu 1.0:aan, alle 0.0:aan.
        assert_eq!(Provenance::external("web", 5.0).trust(), 1.0);
        assert_eq!(Provenance::external("web", -2.0).trust(), 0.0);
        // NaN → 0.0 (turvallinen oletus).
        assert_eq!(Provenance::external("web", f32::NAN).trust(), 0.0);
    }

    #[test]
    fn trust_levels_per_variant() {
        assert_eq!(Provenance::DirectExperience.trust(), 1.0);
        assert_eq!(Provenance::derived([MessageId::new()]).trust(), 1.0);
        assert_eq!(Provenance::external("web", 0.3).trust(), 0.3);
    }

    #[test]
    fn default_is_direct_experience() {
        assert_eq!(Provenance::default(), Provenance::DirectExperience);
        assert!(!Provenance::default().is_external());
    }

    #[test]
    fn gate_default_threshold() {
        let gate = ProvenanceGate::default();
        assert_eq!(gate.min_trust(), 0.5);
    }

    #[test]
    fn gate_min_trust_clamped() {
        assert_eq!(ProvenanceGate::new(2.0).min_trust(), 1.0);
        assert_eq!(ProvenanceGate::new(-1.0).min_trust(), 0.0);
        // Ei-äärellinen kynnys → 0.0 (päästä kaikki).
        assert_eq!(ProvenanceGate::new(f32::INFINITY).min_trust(), 0.0);
    }

    #[test]
    fn is_external_detects_variant() {
        assert!(Provenance::external("web", 0.5).is_external());
        assert!(!Provenance::DirectExperience.is_external());
        assert!(!Provenance::derived([]).is_external());
    }

    #[test]
    fn serde_roundtrip_all_variants() {
        let cases = vec![
            Provenance::DirectExperience,
            Provenance::derived([MessageId::new(), MessageId::new()]),
            Provenance::external("web", 0.42),
        ];
        for p in cases {
            let json = serde_json::to_string(&p).expect("serialize");
            let back: Provenance = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(p, back);
        }
    }

    #[test]
    fn external_serde_uses_generic_source() {
        // Layer-B: lähde on geneerinen, ei perheen nimiä.
        let p = Provenance::external("web", 0.8);
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(json.contains("external"));
        assert!(json.contains("web"));
    }
}

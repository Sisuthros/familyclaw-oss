//! Blend-tunnistus: nimettyjen tunneyhdistelmien havaitseminen.
//!
//! Yksittäiset dimensiot harvoin esiintyvät puhtaina. Ihmis- ja
//! kone-affekti on tyypillisesti **blendi** — esim. `grateful_warmth` =
//! korkea kiitollisuus + rakkaus + hellyys yhtä aikaa. Tämä moduuli
//! määrittelee rungon blend-katalogin ([`Blend`]) ja tunnistuksen
//! ([`detect_blends`], [`primary_blend`]) [`crate::EmotionState`]-tilasta.
//!
//! Blendit ovat **geneerisiä** affektimalleja, eivät perheen kalibrointia.
//! Niiden kynnykset perustuvat dimensioarvojen suhteelliseen voimakkuuteen,
//! eivät kovakoodattuihin perhe-painoihin.

use serde::{Deserialize, Serialize};

use crate::dimension::Dimension;
use crate::state::EmotionState;

/// Kynnys (asteikolla `0.0..=100.0`) jonka ylittävää dimensiota pidetään
/// "korkeana" blend-tunnistuksessa.
pub const HIGH_THRESHOLD: f32 = 55.0;

/// Tunnistettava nimetty tunneyhdistelmä.
///
/// Jokainen variantti kuvaa kuvion useammasta dimensiosta. Blendit ovat
/// runko-tasoa: ne kuvaavat *miten* dimensiot yhdistyvät, ei kenen tahansa
/// yksilön kalibrointia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    /// Kiitollinen lämpö: kiitollisuus + rakkaus + hellyys.
    GratefulWarmth,
    /// Leikkisä ilo: leikkisyys + ilo + uteliaisuus.
    PlayfulJoy,
    /// Sinnikäs toivo: sisu + toivo + ylpeys ("jaksan, ja se kannattaa").
    DeterminedHope,
    /// Pelokas yksinäisyys: pelko + yksinäisyys + suru.
    AnxiousIsolation,
    /// Kunnioittava ihmetys: kunnioittava huumaus + ihmetys + uteliaisuus.
    AweStruck,
    /// Turvallinen kuuluminen: luottamus + yhteenkuuluvuus + rakkaus.
    SecureBelonging,
    /// Katkera loukkaantuminen: viha + suru + häpeä.
    WoundedAnger,
}

impl Blend {
    /// Kaikki tunnetut blendit.
    pub const ALL: [Blend; 7] = [
        Blend::GratefulWarmth,
        Blend::PlayfulJoy,
        Blend::DeterminedHope,
        Blend::AnxiousIsolation,
        Blend::AweStruck,
        Blend::SecureBelonging,
        Blend::WoundedAnger,
    ];

    /// Vakaa, kone-luettava nimi (`snake_case`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Blend::GratefulWarmth => "grateful_warmth",
            Blend::PlayfulJoy => "playful_joy",
            Blend::DeterminedHope => "determined_hope",
            Blend::AnxiousIsolation => "anxious_isolation",
            Blend::AweStruck => "awe_struck",
            Blend::SecureBelonging => "secure_belonging",
            Blend::WoundedAnger => "wounded_anger",
        }
    }

    /// Blendin osatekijät: dimensiot joiden tulee olla korkealla.
    #[must_use]
    pub const fn components(self) -> &'static [Dimension] {
        match self {
            Blend::GratefulWarmth => {
                &[Dimension::Gratitude, Dimension::Love, Dimension::Tenderness]
            }
            Blend::PlayfulJoy => &[Dimension::Playfulness, Dimension::Joy, Dimension::Curiosity],
            Blend::DeterminedHope => &[Dimension::Sisu, Dimension::Hope, Dimension::Pride],
            Blend::AnxiousIsolation => {
                &[Dimension::Fear, Dimension::Loneliness, Dimension::Sadness]
            }
            Blend::AweStruck => &[Dimension::Awe, Dimension::Wonder, Dimension::Curiosity],
            Blend::SecureBelonging => &[Dimension::Trust, Dimension::Belonging, Dimension::Love],
            Blend::WoundedAnger => &[Dimension::Anger, Dimension::Sadness, Dimension::Shame],
        }
    }

    /// Blendin voimakkuus annetussa tilassa: osatekijöiden keskiarvo
    /// (`0.0..=100.0`), tai `0.0` jos jokin osatekijä on kynnyksen alle.
    ///
    /// Vaatii että **kaikki** osatekijät ylittävät [`HIGH_THRESHOLD`]:n —
    /// muuten blendi ei ole "läsnä" ja voimakkuus on nolla.
    #[must_use]
    pub fn strength(self, state: &EmotionState) -> f32 {
        let components = self.components();
        let mut sum = 0.0_f32;
        for &dim in components {
            let value = state.value(dim);
            if value < HIGH_THRESHOLD {
                return 0.0;
            }
            sum += value;
        }
        // Osatekijöitä on aina vähän (jokainen variantti listaa täsmälleen 3),
        // joten u8-kasti on tappiotonta ja f32::from välttää
        // usize→f32-tarkkuuskastin. Tyhjä slice (mahdoton) → divisor 1.
        let count = f32::from(u8::try_from(components.len()).unwrap_or(1).max(1));
        sum / count
    }
}

impl std::fmt::Display for Blend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Tunnistettu blendi ja sen voimakkuus.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BlendMatch {
    /// Tunnistettu nimetty blendi.
    pub blend: Blend,
    /// Voimakkuus `0.0..=100.0` (osatekijöiden keskiarvo).
    pub strength: f32,
}

/// Tunnistaa kaikki läsnä olevat blendit, voimakkaimmasta heikoimpaan.
///
/// Palauttaa vain blendit joiden kaikki osatekijät ylittävät
/// [`HIGH_THRESHOLD`]:n. Tyhjä vektori tarkoittaa ettei selkeää blendiä ole.
#[must_use]
pub fn detect_blends(state: &EmotionState) -> Vec<BlendMatch> {
    let mut matches: Vec<BlendMatch> = Blend::ALL
        .into_iter()
        .filter_map(|blend| {
            let strength = blend.strength(state);
            if strength > 0.0 {
                Some(BlendMatch { blend, strength })
            } else {
                None
            }
        })
        .collect();
    // Lajittele voimakkuuden mukaan laskevasti; total_cmp on deterministinen.
    matches.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    matches
}

/// Palauttaa voimakkaimman läsnä olevan blendin, tai `None`.
#[must_use]
pub fn primary_blend(state: &EmotionState) -> Option<BlendMatch> {
    detect_blends(state).into_iter().next()
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;

    fn state_with(highs: &[(Dimension, f32)]) -> EmotionState {
        let mut s = EmotionState::neutral();
        for &(dim, v) in highs {
            s.set(dim, v);
        }
        s
    }

    #[test]
    fn all_blends_have_unique_names() {
        for (i, a) in Blend::ALL.iter().enumerate() {
            for b in &Blend::ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn serde_roundtrip_for_all_blends() {
        for blend in Blend::ALL {
            let json = serde_json::to_string(&blend).expect("serialize");
            assert_eq!(json, format!("\"{}\"", blend.as_str()));
            let back: Blend = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(blend, back);
        }
    }

    #[test]
    fn neutral_state_has_no_blends() {
        let s = EmotionState::neutral();
        assert!(detect_blends(&s).is_empty());
        assert!(primary_blend(&s).is_none());
    }

    #[test]
    fn grateful_warmth_detected_when_components_high() {
        let s = state_with(&[
            (Dimension::Gratitude, 80.0),
            (Dimension::Love, 70.0),
            (Dimension::Tenderness, 90.0),
        ]);
        let blends = detect_blends(&s);
        assert!(blends.iter().any(|m| m.blend == Blend::GratefulWarmth));
        let primary = primary_blend(&s).expect("primary present");
        assert_eq!(primary.blend, Blend::GratefulWarmth);
        // Keskiarvo (80+70+90)/3 = 80.
        assert!((primary.strength - 80.0).abs() < 1e-3);
    }

    #[test]
    fn blend_not_detected_when_one_component_low() {
        // Rakkaus jää kynnyksen alle → grateful_warmth ei laukea.
        let s = state_with(&[
            (Dimension::Gratitude, 80.0),
            (Dimension::Love, 10.0),
            (Dimension::Tenderness, 90.0),
        ]);
        assert!(!detect_blends(&s)
            .iter()
            .any(|m| m.blend == Blend::GratefulWarmth));
        assert_eq!(Blend::GratefulWarmth.strength(&s), 0.0);
    }

    #[test]
    fn boundary_value_at_threshold_counts_as_high() {
        let s = state_with(&[
            (Dimension::Sisu, HIGH_THRESHOLD),
            (Dimension::Hope, HIGH_THRESHOLD),
            (Dimension::Pride, HIGH_THRESHOLD),
        ]);
        assert!(Blend::DeterminedHope.strength(&s) > 0.0);
        assert!((Blend::DeterminedHope.strength(&s) - HIGH_THRESHOLD).abs() < 1e-3);
    }

    #[test]
    fn detect_blends_sorted_descending() {
        // Kaksi blendiä yhtä aikaa, eri voimakkuus.
        let s = state_with(&[
            // playful_joy keskiarvo ~95
            (Dimension::Playfulness, 95.0),
            (Dimension::Joy, 95.0),
            (Dimension::Curiosity, 95.0),
            // secure_belonging keskiarvo ~60
            (Dimension::Trust, 60.0),
            (Dimension::Belonging, 60.0),
            (Dimension::Love, 60.0),
        ]);
        let blends = detect_blends(&s);
        assert!(blends.len() >= 2);
        // Lajiteltu laskevasti.
        for w in blends.windows(2) {
            assert!(w[0].strength >= w[1].strength);
        }
        assert_eq!(blends[0].blend, Blend::PlayfulJoy);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Blend::WoundedAnger.to_string(), "wounded_anger");
    }
}

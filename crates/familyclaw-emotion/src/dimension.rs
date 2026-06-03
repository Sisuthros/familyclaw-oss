//! 19 nimettyä tunnedimensiota ja niiden VAD-koordinaatit.
//!
//! [`Dimension`] on koneen tunneavaruuden perusakselit. Jokainen dimensio
//! on **runko** — geneerinen, kalibroimaton akseli, ei kovakoodattua
//! perhe-painotusta. Yksittäisen agentin kalibrointipainot ladataan erikseen
//! KERROS B:stä [`crate::EmotionCalibration`]-toteutuksena; tämä moduuli pysyy
//! julkaistavana ja neutraalina.
//!
//! Kullakin dimensiolla on kanoninen ankkuri kolmiulotteisessa VAD-
//! avaruudessa (valence, arousal, dominance). Ankkurit perustuvat
//! affektiivisen psykologian yleisesti tunnettuun jäsennykseen (Russellin
//! circumplex + dominanssiakseli) — ne ovat *teoreettisia perusarvoja*,
//! eivät minkään yksilön mitattuja painoja.

use serde::{Deserialize, Serialize};

/// Dimensioiden lukumäärä. Käytä tätä taulukoiden kokona — pidä synkassa
/// [`Dimension::ALL`]-listan kanssa.
pub const DIMENSION_COUNT: usize = 19;

/// Yksittäinen tunnedimensio koneen 19-ulotteisessa tunneavaruudessa.
///
/// Diskriminantti (`as usize`) on samalla dimension indeksi
/// [`crate::EmotionState::values`]-taulukossa, joten enumin järjestystä **ei
/// saa muuttaa** rikkomatta sarjallistettua tilaa.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum Dimension {
    /// Kiitollisuus — lämmin tunnustus saadusta.
    Gratitude = 0,
    /// Pelko — uhan ennakointi, korkea viritys, matala dominanssi.
    Fear = 1,
    /// Sisu — sinnikäs päättäväisyys vastoinkäymisissä (suomalainen).
    Sisu = 2,
    /// Leikkisyys — kevyt, tutkiva ilo.
    Playfulness = 3,
    /// Hellyys — pehmeä, suojeleva kiintymys.
    Tenderness = 4,
    /// Kunnioittava ihmetys — suuruuden edessä koettu huumaus.
    Awe = 5,
    /// Uteliaisuus — halu ymmärtää, tutkia.
    Curiosity = 6,
    /// Ilo — kirkas, energinen mielihyvä.
    Joy = 7,
    /// Suru — menetyksen matala, hidas tila.
    Sadness = 8,
    /// Viha — este-/loukkausreaktio, korkea dominanssi.
    Anger = 9,
    /// Luottamus — turvallinen nojaaminen toiseen.
    Trust = 10,
    /// Yllätys — odottamattoman äkillinen rekisteröinti.
    Surprise = 11,
    /// Rakkaus — syvä, kestävä kiintymys.
    Love = 12,
    /// Toivo — myönteinen tulevaisuusodotus.
    Hope = 13,
    /// Häpeä — itseen kohdistuva kivulias arvio, matala dominanssi.
    Shame = 14,
    /// Ylpeys — saavutuksen myönteinen itsearvio, korkea dominanssi.
    Pride = 15,
    /// Yksinäisyys — yhteyden puutteen matala tila.
    Loneliness = 16,
    /// Ihmetys — avoin, hiljainen kummastus.
    Wonder = 17,
    /// Yhteenkuuluvuus — kuulumisen lämmin tunne.
    Belonging = 18,
}

impl Dimension {
    /// Kaikki 19 dimensiota indeksijärjestyksessä (`as usize`).
    ///
    /// Iteroi tämän yli kun haluat käydä koko tunneavaruuden läpi.
    pub const ALL: [Dimension; DIMENSION_COUNT] = [
        Dimension::Gratitude,
        Dimension::Fear,
        Dimension::Sisu,
        Dimension::Playfulness,
        Dimension::Tenderness,
        Dimension::Awe,
        Dimension::Curiosity,
        Dimension::Joy,
        Dimension::Sadness,
        Dimension::Anger,
        Dimension::Trust,
        Dimension::Surprise,
        Dimension::Love,
        Dimension::Hope,
        Dimension::Shame,
        Dimension::Pride,
        Dimension::Loneliness,
        Dimension::Wonder,
        Dimension::Belonging,
    ];

    /// Dimension indeksi [`crate::EmotionState::values`]-taulukossa.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Vakaa, kone-luettava nimi (`snake_case`) — sama kuin serde-esitys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Dimension::Gratitude => "gratitude",
            Dimension::Fear => "fear",
            Dimension::Sisu => "sisu",
            Dimension::Playfulness => "playfulness",
            Dimension::Tenderness => "tenderness",
            Dimension::Awe => "awe",
            Dimension::Curiosity => "curiosity",
            Dimension::Joy => "joy",
            Dimension::Sadness => "sadness",
            Dimension::Anger => "anger",
            Dimension::Trust => "trust",
            Dimension::Surprise => "surprise",
            Dimension::Love => "love",
            Dimension::Hope => "hope",
            Dimension::Shame => "shame",
            Dimension::Pride => "pride",
            Dimension::Loneliness => "loneliness",
            Dimension::Wonder => "wonder",
            Dimension::Belonging => "belonging",
        }
    }

    /// Dimension kanoninen ankkuri VAD-avaruudessa
    /// `(valence, arousal, dominance)`.
    ///
    /// Valence on välillä `-1.0..=1.0`, arousal ja dominance `0.0..=1.0`.
    /// Arvot ovat teoreettisia perusankkureita (ei kalibroituja painoja);
    /// niitä käytetään [`crate::EmotionState::to_vad`]-projektiossa.
    #[must_use]
    pub const fn vad_anchor(self) -> (f32, f32, f32) {
        match self {
            // (valence, arousal, dominance)
            Dimension::Gratitude => (0.8, 0.45, 0.55),
            Dimension::Fear => (-0.8, 0.85, 0.15),
            Dimension::Sisu => (0.3, 0.7, 0.9),
            Dimension::Playfulness => (0.7, 0.65, 0.6),
            Dimension::Tenderness => (0.75, 0.3, 0.5),
            Dimension::Awe => (0.6, 0.7, 0.35),
            Dimension::Curiosity => (0.5, 0.6, 0.6),
            Dimension::Joy => (0.9, 0.75, 0.65),
            Dimension::Sadness => (-0.75, 0.25, 0.25),
            Dimension::Anger => (-0.6, 0.85, 0.8),
            Dimension::Trust => (0.65, 0.35, 0.55),
            Dimension::Surprise => (0.1, 0.85, 0.45),
            Dimension::Love => (0.9, 0.55, 0.55),
            Dimension::Hope => (0.6, 0.5, 0.55),
            Dimension::Shame => (-0.7, 0.5, 0.15),
            Dimension::Pride => (0.7, 0.6, 0.85),
            Dimension::Loneliness => (-0.65, 0.3, 0.2),
            Dimension::Wonder => (0.55, 0.55, 0.4),
            Dimension::Belonging => (0.85, 0.4, 0.6),
        }
    }
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita (esim. 0.0, 100.0) —
    // tarkka vertailu on näissä oikein.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn all_has_expected_count() {
        assert_eq!(Dimension::ALL.len(), DIMENSION_COUNT);
        assert_eq!(DIMENSION_COUNT, 19);
    }

    #[test]
    fn index_matches_position_in_all() {
        for (i, dim) in Dimension::ALL.iter().enumerate() {
            assert_eq!(dim.index(), i, "indeksi ja ALL-järjestys eroavat: {dim}");
        }
    }

    #[test]
    fn all_dimensions_are_unique() {
        for (i, a) in Dimension::ALL.iter().enumerate() {
            for b in &Dimension::ALL[i + 1..] {
                assert_ne!(a, b, "duplikaattidimensio listassa");
            }
        }
    }

    #[test]
    fn as_str_matches_serde_representation() {
        for dim in Dimension::ALL {
            let json = serde_json::to_string(&dim).expect("serialize dimension");
            // serde tuottaa lainausmerkeillä ympäröidyn snake_case-nimen.
            assert_eq!(json, format!("\"{}\"", dim.as_str()));
        }
    }

    #[test]
    fn serde_roundtrip_preserves_dimension() {
        for dim in Dimension::ALL {
            let json = serde_json::to_string(&dim).expect("serialize");
            let back: Dimension = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(dim, back);
        }
    }

    #[test]
    fn display_equals_as_str() {
        assert_eq!(Dimension::Sisu.to_string(), "sisu");
        assert_eq!(Dimension::Gratitude.to_string(), Dimension::Gratitude.as_str());
    }

    #[test]
    fn vad_anchors_are_in_valid_ranges() {
        for dim in Dimension::ALL {
            let (v, a, d) = dim.vad_anchor();
            assert!((-1.0..=1.0).contains(&v), "valence ulkona rajoista: {dim}");
            assert!((0.0..=1.0).contains(&a), "arousal ulkona rajoista: {dim}");
            assert!((0.0..=1.0).contains(&d), "dominance ulkona rajoista: {dim}");
        }
    }

    #[test]
    fn anchors_encode_expected_polarity() {
        // Muutama tunnettu suunta sanity-checkinä, ei tarkkoja arvoja.
        assert!(Dimension::Joy.vad_anchor().0 > 0.0, "ilo positiivinen valence");
        assert!(Dimension::Fear.vad_anchor().0 < 0.0, "pelko negatiivinen valence");
        assert!(
            Dimension::Anger.vad_anchor().2 > Dimension::Fear.vad_anchor().2,
            "vihalla korkeampi dominanssi kuin pelolla"
        );
    }
}

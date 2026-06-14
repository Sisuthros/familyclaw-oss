//! Muistin tärkeyden laskenta (composite importance).
//!
//! Kuinka tärkeä muisto on määrää sekä sen säilyvyyden (vahvuus `S`
//! Ebbinghaus-kaavassa) että sen sijoittumisen haussa. Tärkeys lasketaan
//! neljästä painotetusta osatekijästä (`FamilyClaw` v2 design §5, Eternal
//! Thread `RUST_ARCHITECTURE.md` "Ebbinghaus Scoring"):
//!
//! ```text
//! importance = emotion       · 0.45
//!            + identity       · 0.35
//!            + novelty        · 0.12
//!            + reinforcement  · 0.20
//! ```
//!
//! Jokainen osatekijä on välillä `0.0..=1.0`. Painot eivät summaudu yhteen
//! (Σ = 1.12) — tämä on tarkoituksellista, jotta vahvasti latautunut muisto
//! voi ylittää neutraalin perustason. Lopullinen tärkeys puristetaan
//! välille `0.0..=1.0`.
//!
//! **OSS-raja (KERROS A):** tämä moduuli sisältää vain laskennan *rungon*.
//! Mitään perheenjäsenen kalibrointia (esim. mitkä sanat ovat
//! identiteetille tärkeitä) ei kovakoodata tähän — osatekijät annetaan
//! sisään valmiiksi laskettuina.

use familyclaw_emotion::{emotional_salience, EmotionState};
use serde::{Deserialize, Serialize};

/// Tunnelatauksen paino tärkeydessä.
pub const WEIGHT_EMOTION: f32 = 0.45;
/// Identiteettiosuvuuden paino tärkeydessä.
pub const WEIGHT_IDENTITY: f32 = 0.35;
/// Uutuuden (novelty) paino tärkeydessä.
pub const WEIGHT_NOVELTY: f32 = 0.12;
/// Vahvistuksen (reinforcement) paino tärkeydessä.
pub const WEIGHT_REINFORCEMENT: f32 = 0.20;

/// Tärkeyden osatekijät, kukin välillä `0.0..=1.0`.
///
/// Kentät kuvaavat *miksi* muisto on tärkeä. Ne lasketaan ajonaikaisesti
/// (tunnetilasta, identiteettiosumasta, uutuudesta, vahvistuksesta) ja
/// yhdistetään painotetusti [`ImportanceFactors::composite`]-metodilla.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImportanceFactors {
    /// Tunnelataus: kuinka voimakkaasti muisto on emotionaalisesti
    /// virittynyt (esim. johdettu VAD-magnitudista). `0.0..=1.0`.
    pub emotion: f32,
    /// Identiteettiosuvuus: kuinka lähellä muisto on olennon identiteettiä
    /// / kantavia arvoja. `0.0..=1.0`.
    pub identity: f32,
    /// Uutuus: kuinka tuore/odottamaton tieto muisto on suhteessa
    /// olemassa olevaan. `0.0..=1.0`.
    pub novelty: f32,
    /// Vahvistus: kuinka monta kertaa muisto on toistunut/aktivoitunut
    /// (normalisoituna). `0.0..=1.0`.
    pub reinforcement: f32,
}

impl ImportanceFactors {
    /// Neutraali (kaikki nollassa) — johtaa minimitärkeyteen.
    pub const ZERO: ImportanceFactors = ImportanceFactors {
        emotion: 0.0,
        identity: 0.0,
        novelty: 0.0,
        reinforcement: 0.0,
    };

    /// Rakentaa osatekijät puristaen jokaisen välille `0.0..=1.0`
    /// (NaN → 0.0).
    #[must_use]
    pub fn new(emotion: f32, identity: f32, novelty: f32, reinforcement: f32) -> Self {
        Self {
            emotion: unit(emotion),
            identity: unit(identity),
            novelty: unit(novelty),
            reinforcement: unit(reinforcement),
        }
    }

    /// Rakentaa osatekijät tunnetilasta: `emotion`-osatekijä johdetaan
    /// [`emotional_salience`]-funktiolla annetusta [`EmotionState`]:stä, muut
    /// kolme osatekijää annetaan sisään valmiiksi laskettuina.
    ///
    /// Tämä on PKG-B-silta tunnemoottorin ja muistin tärkeyden välillä:
    /// voimakkaasti latautunut hetki (korkea salience) johtaa korkeampaan
    /// emotion-osatekijään ja siten vahvempaan, hitaammin unohtuvaan muistoon
    /// (Dynamic Affective Memory, arXiv 2510.27418).
    ///
    /// [`ImportanceFactors`] pysyy tahallaan "litteänä" (`Copy + serde`): tila
    /// projisoidaan yhdeksi `f32`:ksi eikä koko [`EmotionState`]:ä upoteta
    /// osatekijöihin. Kaikki neljä arvoa puristetaan välille `0.0..=1.0`
    /// ([`new`]-semantiikalla; NaN → 0.0).
    ///
    /// [`new`]: ImportanceFactors::new
    #[must_use]
    pub fn from_emotion_state(
        state: &EmotionState,
        identity: f32,
        novelty: f32,
        reinforcement: f32,
    ) -> Self {
        Self::new(emotional_salience(state), identity, novelty, reinforcement)
    }

    /// Laskee painotetun yhdistelmätärkeyden, puristettuna `0.0..=1.0`.
    ///
    /// Painot: emotion 0.45, identity 0.35, novelty 0.12, reinforcement 0.20.
    /// Osatekijät puristetaan ennen laskentaa, joten tulos on aina
    /// kelvollinen vaikka kentät olisi asetettu suoraan (ilman [`new`]).
    ///
    /// [`new`]: ImportanceFactors::new
    #[must_use]
    pub fn composite(&self) -> f32 {
        let e = unit(self.emotion);
        let i = unit(self.identity);
        let n = unit(self.novelty);
        let r = unit(self.reinforcement);
        let raw = e.mul_add(
            WEIGHT_EMOTION,
            i.mul_add(
                WEIGHT_IDENTITY,
                n.mul_add(WEIGHT_NOVELTY, r * WEIGHT_REINFORCEMENT),
            ),
        );
        raw.clamp(0.0, 1.0)
    }

    /// Muistin vahvuus `S` Ebbinghaus-retentiokaavaan.
    ///
    /// Tärkeämmät muistot ovat vahvempia ja siten säilyvät pidempään.
    /// Vahvuus skaalautuu lineaarisesti tärkeydestä välille
    /// `min_stability..=max_stability`, jotta neutraalikin muisto saa
    /// jonkin perussäilyvyyden.
    ///
    /// `min_stability` ja `max_stability` puristetaan järkeviin rajoihin;
    /// jos `max < min`, ne vaihdetaan keskenään.
    #[must_use]
    pub fn stability(&self, min_stability: f32, max_stability: f32) -> f32 {
        let (lo, hi) = ordered_positive(min_stability, max_stability);
        let importance = self.composite();
        importance.mul_add(hi - lo, lo)
    }
}

impl Default for ImportanceFactors {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Puristaa arvon välille `0.0..=1.0`; NaN → 0.0.
fn unit(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Palauttaa kaksi ei-negatiivista, äärellistä arvoa nousevassa
/// järjestyksessä. Kelvottomat arvot korvataan turvallisilla oletuksilla.
fn ordered_positive(a: f32, b: f32) -> (f32, f32) {
    let sa = if a.is_finite() && a > 0.0 { a } else { 0.05 };
    let sb = if b.is_finite() && b > 0.0 { b } else { 1.0 };
    if sa <= sb {
        (sa, sb)
    } else {
        (sb, sa)
    }
}

#[cfg(test)]
mod tests {
    // Testit vertaavat tarkasti esitettäviä f32-vakioita — tarkka vertailu ok.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn weights_match_design() {
        assert_eq!(WEIGHT_EMOTION, 0.45);
        assert_eq!(WEIGHT_IDENTITY, 0.35);
        assert_eq!(WEIGHT_NOVELTY, 0.12);
        assert_eq!(WEIGHT_REINFORCEMENT, 0.20);
    }

    #[test]
    fn zero_factors_give_zero_importance() {
        assert_eq!(ImportanceFactors::ZERO.composite(), 0.0);
        assert_eq!(ImportanceFactors::default().composite(), 0.0);
    }

    #[test]
    fn composite_uses_exact_weights() {
        let f = ImportanceFactors::new(1.0, 0.0, 0.0, 0.0);
        assert!((f.composite() - WEIGHT_EMOTION).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 1.0, 0.0, 0.0);
        assert!((f.composite() - WEIGHT_IDENTITY).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 0.0, 1.0, 0.0);
        assert!((f.composite() - WEIGHT_NOVELTY).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 0.0, 0.0, 1.0);
        assert!((f.composite() - WEIGHT_REINFORCEMENT).abs() < 1e-6);
    }

    #[test]
    fn composite_combines_factors() {
        // emotion 0.5, identity 0.5, novelty 0.5, reinforcement 0.5
        // = 0.5·(0.45+0.35+0.12+0.20) = 0.5·1.12 = 0.56.
        let f = ImportanceFactors::new(0.5, 0.5, 0.5, 0.5);
        assert!((f.composite() - 0.56).abs() < 1e-5);
    }

    #[test]
    fn composite_clamps_to_unit_when_all_max() {
        // Σ painot = 1.12 → puristuu 1.0:aan.
        let f = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(f.composite(), 1.0);
    }

    #[test]
    fn new_clamps_and_sanitizes_inputs() {
        let f = ImportanceFactors::new(5.0, -3.0, f32::NAN, 0.5);
        assert_eq!(f.emotion, 1.0);
        assert_eq!(f.identity, 0.0);
        assert_eq!(f.novelty, 0.0);
        assert_eq!(f.reinforcement, 0.5);
    }

    #[test]
    fn composite_sanitizes_directly_set_fields() {
        // Kentät asetettu suoraan ohi konstruktorin — composite puristaa silti.
        let f = ImportanceFactors {
            emotion: 10.0,
            identity: -1.0,
            novelty: f32::NAN,
            reinforcement: 2.0,
        };
        let c = f.composite();
        assert!((0.0..=1.0).contains(&c));
        // emotion→1.0, reinforcement→1.0, muut 0 → 0.45 + 0.20 = 0.65.
        assert!((c - 0.65).abs() < 1e-5);
    }

    #[test]
    fn stability_scales_with_importance() {
        let low = ImportanceFactors::new(0.1, 0.0, 0.0, 0.0);
        let high = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        let s_low = low.stability(0.5, 5.0);
        let s_high = high.stability(0.5, 5.0);
        assert!(s_high > s_low);
        // Maksimitärkeys → max_stability.
        assert!((s_high - 5.0).abs() < 1e-5);
        // Tärkeys 0 → min_stability.
        let s_zero = ImportanceFactors::ZERO.stability(0.5, 5.0);
        assert!((s_zero - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stability_swaps_inverted_bounds() {
        let f = ImportanceFactors::new(0.0, 0.0, 0.0, 0.0);
        // max < min → vaihdetaan; tärkeys 0 → pienempi raja.
        let s = f.stability(5.0, 0.5);
        assert!((s - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stability_handles_invalid_bounds() {
        let f = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        let s = f.stability(f32::NAN, -2.0);
        assert!(s.is_finite());
        assert!(s > 0.0);
    }

    #[test]
    fn serde_roundtrip() {
        let f = ImportanceFactors::new(0.3, 0.7, 0.1, 0.9);
        let json = serde_json::to_string(&f).expect("serialize");
        let back: ImportanceFactors = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }

    // ── PKG-B: tunne → tärkeys -silta ──────────────────────────────────────

    #[test]
    fn from_emotion_state_derives_emotion_factor_from_salience() {
        use familyclaw_emotion::{Dimension, EmotionState};

        let mut state = EmotionState::neutral();
        state.set(Dimension::Joy, 95.0);
        let salience = emotional_salience(&state);

        let f = ImportanceFactors::from_emotion_state(&state, 0.2, 0.3, 0.4);
        // emotion-osatekijä = salience (puristettuna).
        assert!((f.emotion - salience).abs() < 1e-6);
        // Muut kentät tulevat suoraan parametreista.
        assert_eq!(f.identity, 0.2);
        assert_eq!(f.novelty, 0.3);
        assert_eq!(f.reinforcement, 0.4);
    }

    #[test]
    fn salience_derived_importance_differs_from_neutral() {
        use familyclaw_emotion::{Dimension, EmotionState};

        // Voimakkaasti latautunut hetki vs. neutraali — samat muut osatekijät.
        let neutral = EmotionState::neutral();
        let mut charged = EmotionState::neutral();
        charged.set(Dimension::Joy, 95.0);

        let f_neutral = ImportanceFactors::from_emotion_state(&neutral, 0.0, 0.0, 0.0);
        let f_charged = ImportanceFactors::from_emotion_state(&charged, 0.0, 0.0, 0.0);

        // Latautunut hetki saa korkeamman emotion-osatekijän → korkeamman
        // yhdistelmätärkeyden kuin neutraali.
        assert!(f_charged.emotion > f_neutral.emotion);
        assert!(
            f_charged.composite() > f_neutral.composite(),
            "salienssista johdetun tärkeyden pitäisi ylittää neutraali"
        );
    }

    #[test]
    fn from_emotion_state_sanitizes_other_factors() {
        use familyclaw_emotion::EmotionState;

        // Kelvottomat muut osatekijät puristetaan (new-semantiikka).
        let state = EmotionState::neutral();
        let f = ImportanceFactors::from_emotion_state(&state, 5.0, -3.0, f32::NAN);
        assert_eq!(f.identity, 1.0);
        assert_eq!(f.novelty, 0.0);
        assert_eq!(f.reinforcement, 0.0);
        assert!((0.0..=1.0).contains(&f.emotion));
    }

    #[test]
    fn from_emotion_state_is_copy_and_serde() {
        use familyclaw_emotion::{Dimension, EmotionState};

        let mut state = EmotionState::neutral();
        state.set(Dimension::Fear, 90.0);
        let f = ImportanceFactors::from_emotion_state(&state, 0.5, 0.5, 0.5);
        // Copy: kopio ei kuluta alkuperäistä.
        let copy = f;
        assert_eq!(f, copy);
        // Serde-roundtrip säilyy (litteä tyyppi, ei upotettua EmotionStatea).
        let json = serde_json::to_string(&f).expect("serialize");
        let back: ImportanceFactors = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }
}

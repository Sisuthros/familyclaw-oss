//! Tunteen *tärkeyspaino* (affective salience) — kuinka "muistamisen
//! arvoinen" hetkellinen tunnetila on.
//!
//! Tämä moduuli tarjoaa yhden puhtaan funktion,
//! [`emotional_salience`], joka projisoi [`EmotionState`]:n yhdeksi luvuksi
//! välillä `0.0..=1.0`. Idea on inspiroitu *Dynamic Affective Memory*
//! -työstä (arXiv 2510.27418): voimakkaasti latautuneet hetket — korkea
//! viritys yhdistettynä selvään valenssin etumerkkiin — ovat
//! tärkeämpiä säilyttää kuin laimeat, neutraalit hetket.
//!
//! ## Cross-crate-kytkentä (TODO)
//! Tämä crate **ei** kutsu mitään muuta cratea. `familyclaw-memory` voi
//! MYÖHEMMIN kutsua [`emotional_salience`]-funktiota painottaakseen
//! muistojen tärkeyttä (importance weight) — mutta sitä kytkentää **ei**
//! tehdä täällä. Tarjolla on vain puhdas funktio.
//!
//! TODO(memory): `familyclaw-memory` voi kutsua tätä
//! importance-painona muistin tallennuksessa / decayssa.

use crate::state::EmotionState;

/// Suurin dimensioarvo (asteikko `0.0..=100.0`).
const VALUE_MAX: f32 = 100.0;

/// Arvioi tunnetilan *tärkeyden* välillä `0.0..=1.0`.
///
/// Salience on korkea kun tila on **voimakas** ja **selvästi latautunut**
/// — eli kun sekä viritys (arousal) että valenssin itseisarvo ovat
/// suuria. Neutraali tila → lähellä nollaa; voimakas ilo TAI voimakas
/// pelko → lähellä ykköstä.
///
/// ## Malli (kevyt, läpinäkyvä)
/// Salience yhdistää kolme signaalia VAD-projektiosta
/// ([`EmotionState::to_vad`]) ja tilan raa'asta intensiteetistä:
///
/// 1. **Viritys** (`arousal`, `0.0..=1.0`) — kuinka kiihtynyt hetki on.
/// 2. **Valenssin voimakkuus** (`|valence|`, `0.0..=1.0`) — kuinka
///    selvästi positiivinen tai negatiivinen (etumerkki ei vaikuta:
///    sekä huiput että pohjat ovat muistamisen arvoisia).
/// 3. **Intensiteetti** (suurin dimensioarvo skaalattuna `0.0..=1.0`) —
///    täysin laimea tila ei ole tärkeä vaikka sen ankkurit osoittaisivat
///    ääripäihin.
///
/// Tulos on näiden painotettu yhdistelmä, puristettuna `0.0..=1.0`.
/// Painot suosivat virityksen ja valenssin voimakkuuden yhdistelmää
/// (Dynamic Affective Memory: latautuneet hetket jäävät mieleen), mutta
/// intensiteetti toimii porttina ettei lähes-neutraali tila saa korkeaa
/// arvoa.
///
/// NaN-syöte on jo siivottu [`EmotionState`]:n sisällä (arvot ovat aina
/// rajoissa), joten tulos on aina äärellinen.
///
/// # Esimerkki
/// ```
/// use familyclaw_emotion::{Dimension, EmotionState, emotional_salience};
///
/// // Neutraali tila → matala salience.
/// let neutral = EmotionState::neutral();
/// assert!(emotional_salience(&neutral) < 0.1);
///
/// // Voimakas ilo → korkea salience.
/// let mut joyful = EmotionState::neutral();
/// joyful.set(Dimension::Joy, 95.0);
/// assert!(emotional_salience(&joyful) > emotional_salience(&neutral));
/// ```
#[must_use]
pub fn emotional_salience(state: &EmotionState) -> f32 {
    let vad = state.to_vad();

    // Viritys on jo 0..1.
    let arousal = vad.arousal.clamp(0.0, 1.0);
    // Valenssin voimakkuus: ääripäät (kumpikin etumerkki) ovat tärkeitä.
    let valence_mag = vad.valence.abs().clamp(0.0, 1.0);

    // Raaka intensiteetti = suurin dimensioarvo skaalattuna 0..1.
    let intensity = state
        .dominant()
        .map_or(0.0, |(_, v)| (v / VALUE_MAX).clamp(0.0, 1.0));

    // Latauskomponentti: viritys ja valenssin voimakkuus yhdessä.
    // Käytä keskiarvoa niin että kumpikin yksinään nostaa salienssia,
    // mutta molemmat yhdessä nostavat sen korkeimmalle.
    let charge = f32::midpoint(arousal, valence_mag);

    // Intensiteetti toimii porttina: lähes-neutraali tila vaimentaa
    // tuloksen vaikka ankkurit osoittaisivat ääripäihin.
    let raw = charge * intensity;

    raw.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    // Tarkat f32-vertailut ovat näissä tietoisesti sallittuja.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::dimension::Dimension;

    #[test]
    fn neutral_state_has_low_salience() {
        let s = EmotionState::neutral();
        assert!(emotional_salience(&s) < 0.05);
    }

    #[test]
    fn salience_is_within_unit_range() {
        // Käy läpi muutama ääritila — tulos pysyy aina 0..1.
        for dim in Dimension::ALL {
            let mut s = EmotionState::neutral();
            s.set(dim, 100.0);
            let v = emotional_salience(&s);
            assert!((0.0..=1.0).contains(&v), "salience {v} ulkona rajoista: {dim}");
        }
    }

    #[test]
    fn high_arousal_extreme_valence_is_salient() {
        // Voimakas ilo: korkea viritys + selvä positiivinen valenssi.
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 95.0);
        assert!(
            emotional_salience(&s) > 0.4,
            "voimakkaan ilon pitäisi olla salientti"
        );
    }

    #[test]
    fn strong_negative_is_also_salient() {
        // Valenssin etumerkki ei saa vaikuttaa: pelko on yhtä muistamisen
        // arvoinen kuin ilo.
        let mut fear = EmotionState::neutral();
        fear.set(Dimension::Fear, 95.0);
        assert!(
            emotional_salience(&fear) > 0.4,
            "voimakkaan pelon pitäisi olla salientti"
        );
    }

    #[test]
    fn stronger_intensity_raises_salience() {
        // Sama dimensio, eri voimakkuus → suurempi voimakkuus = suurempi
        // salience (intensiteettiportti toimii).
        let mut weak = EmotionState::neutral();
        weak.set(Dimension::Joy, 20.0);
        let mut strong = EmotionState::neutral();
        strong.set(Dimension::Joy, 90.0);
        assert!(emotional_salience(&strong) > emotional_salience(&weak));
    }

    #[test]
    fn low_arousal_calm_dimension_is_less_salient_than_high_arousal() {
        // Hellyys on matalan virityksen lämpö; ilo on korkean virityksen.
        // Samalla intensiteetillä korkeampi viritys → korkeampi salience.
        let mut calm = EmotionState::neutral();
        calm.set(Dimension::Tenderness, 90.0);
        let mut excited = EmotionState::neutral();
        excited.set(Dimension::Joy, 90.0);
        assert!(
            emotional_salience(&excited) > emotional_salience(&calm),
            "korkean virityksen tilan pitäisi olla salientimpi"
        );
    }

    #[test]
    fn salience_is_deterministic() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Anger, 80.0);
        s.set(Dimension::Pride, 60.0);
        let a = emotional_salience(&s);
        let b = emotional_salience(&s);
        assert_eq!(a, b);
    }
}

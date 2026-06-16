//! Tunnetilan *inertia* — kevyt HMM-tyylinen tilasiirtymä.
//!
//! Tämä moduuli on **inspiroitu** EQ-Negotiatorin (arXiv 2511.03370)
//! HMM-pohjaisesta tunnetilan seurannasta, mutta se **ei ole täysi
//! piilotettu Markov-malli**: ei emissiojakaumia, ei Viterbiä, ei
//! Baum–Welch-opetusta. Tarjolla on yksi kevyt idea: *emotionaalinen
//! inertia* — edellinen tunnetila vaikuttaa seuraavaan, jottei affekti
//! hyppää epäuskottavasti hetkestä toiseen.
//!
//! ## Idea
//! HMM:ssä tilan siirtymätodennäköisyys suosii pysymistä samassa
//! tilassa (diagonaalin painotus). Tässä mallinnamme saman jatkuvana:
//! seuraava tila on *interpolaatio* edellisen (priori) ja havaitun uuden
//! (evidenssi) tilan välillä. `inertia` säätää kuinka paljon menneisyys
//! painaa.

use crate::dimension::Dimension;
use crate::state::EmotionState;

/// Kevyt tunnetilan siirtymä, joka mallintaa emotionaalisen inertian.
///
/// Käytä [`EmotionTransition::new`] jotta `inertia` puristetaan rajoihin
/// `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmotionTransition {
    /// Kuinka paljon edellinen tila painaa seuraavassa, `0.0..=1.0`.
    /// `0.0` = ei inertiaa (uusi havainto korvaa kokonaan), `1.0` =
    /// täysi inertia (tila ei muutu lainkaan). Tyypillinen arvo ~0.6.
    inertia: f32,
}

impl EmotionTransition {
    /// Rakentaa siirtymän annetulla inertialla (puristetaan `0.0..=1.0`,
    /// NaN → [`DEFAULT_INERTIA`]).
    #[must_use]
    pub fn new(inertia: f32) -> Self {
        Self {
            inertia: if inertia.is_finite() {
                inertia.clamp(0.0, 1.0)
            } else {
                DEFAULT_INERTIA
            },
        }
    }

    /// Rungon oletusinertialla rakennettu siirtymä.
    #[must_use]
    pub fn balanced() -> Self {
        Self::new(DEFAULT_INERTIA)
    }

    /// Aktiivinen inertia-arvo (`0.0..=1.0`).
    #[must_use]
    pub const fn inertia(self) -> f32 {
        self.inertia
    }

    /// Yhdistää edellisen tilan (`prev`) ja havaitun uuden tilan
    /// (`observed`) inertian mukaan.
    ///
    /// Jokaiselle dimensiolle:
    /// `next = inertia * prev + (1 - inertia) * observed`.
    ///
    /// Suuri inertia → seuraava tila pysyy lähellä edellistä (hidas,
    /// uskottava muutos). Pieni inertia → uusi havainto vie nopeasti.
    /// Tulos puristetaan [`EmotionState`]:n omiin rajoihin
    /// (`from_values` siivoaa arvot).
    #[must_use]
    pub fn blend(self, prev: &EmotionState, observed: &EmotionState) -> EmotionState {
        let keep = self.inertia;
        let take = 1.0 - keep;
        let mut next = EmotionState::neutral();
        for dim in Dimension::ALL {
            let p = prev.value(dim);
            let o = observed.value(dim);
            // p*keep + o*take, ilmaistuna mul_add-muodossa.
            let v = o.mul_add(take, p * keep);
            next.set(dim, v);
        }
        next
    }
}

impl Default for EmotionTransition {
    fn default() -> Self {
        Self::balanced()
    }
}

/// Rungon oletusinertia — kohtuullinen "menneisyys painaa, muttei lukitse".
pub const DEFAULT_INERTIA: f32 = 0.6;

#[cfg(test)]
mod tests {
    // Tarkat f32-vertailut ovat näissä tietoisesti sallittuja.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn new_clamps_and_sanitizes_inertia() {
        assert_eq!(EmotionTransition::new(5.0).inertia(), 1.0);
        assert_eq!(EmotionTransition::new(-1.0).inertia(), 0.0);
        assert_eq!(EmotionTransition::new(f32::NAN).inertia(), DEFAULT_INERTIA);
        assert_eq!(EmotionTransition::default().inertia(), DEFAULT_INERTIA);
    }

    #[test]
    fn zero_inertia_takes_observation_fully() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 0.0);
            s.set(Dimension::Sadness, 80.0);
            s
        };
        let next = EmotionTransition::new(0.0).blend(&prev, &observed);
        assert_eq!(next, observed, "inertia 0 → havainto korvaa kokonaan");
    }

    #[test]
    fn full_inertia_keeps_previous() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Love, 70.0);
            s
        };
        let observed = EmotionState::neutral();
        let next = EmotionTransition::new(1.0).blend(&prev, &observed);
        assert_eq!(next, prev, "inertia 1 → tila ei muutu");
    }

    #[test]
    fn half_inertia_is_midpoint() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = EmotionState::neutral();
        let next = EmotionTransition::new(0.5).blend(&prev, &observed);
        assert!((next.value(Dimension::Joy) - 50.0).abs() < 1e-4);
    }

    #[test]
    fn inertia_slows_change() {
        // Suurempi inertia → seuraava tila lähempänä edellistä.
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = EmotionState::neutral();
        let slow = EmotionTransition::new(0.8).blend(&prev, &observed);
        let fast = EmotionTransition::new(0.2).blend(&prev, &observed);
        assert!(
            slow.value(Dimension::Joy) > fast.value(Dimension::Joy),
            "korkea inertia pitää tilan lähempänä edellistä"
        );
    }

    #[test]
    fn blend_result_stays_in_bounds() {
        // Vaikka molemmat ääripäissä, tulos pysyy 0..100.
        let prev = EmotionState::from_values([100.0; crate::dimension::DIMENSION_COUNT]);
        let observed = EmotionState::neutral();
        let next = EmotionTransition::balanced().blend(&prev, &observed);
        for dim in Dimension::ALL {
            let v = next.value(dim);
            assert!(
                (0.0..=100.0).contains(&v),
                "arvo {v} ulkona rajoista: {dim}"
            );
        }
    }

    #[test]
    fn repeated_application_converges_toward_observation() {
        // Toistuva sama havainto → tila lähestyy havaintoa (inertia <1).
        let observed = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Hope, 90.0);
            s
        };
        let mut state = EmotionState::neutral();
        let t = EmotionTransition::new(0.5);
        for _ in 0..20 {
            state = t.blend(&state, &observed);
        }
        assert!(
            (state.value(Dimension::Hope) - 90.0).abs() < 0.5,
            "toistuva havainto vetää tilan lähelle havaintoa"
        );
    }
}
